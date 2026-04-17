#!/usr/bin/env bash
# Convergence gate condition script — bridges worker→judge handoff.
#
# Called by gc convergence with gate_mode=condition. After a worker completes,
# this script reads the commit range from bead metadata, nudges the judge
# session, polls for the review verdict, and returns the result.
#
# Exit codes:
#   0 — judge approved (convergence terminates successfully)
#   1 — judge rejected (convergence iterates or escalates)
#
# Environment variables (set by formula env configuration):
#   GC_BEAD_ID               — bead being reviewed (required)
#   GC_POLL_INTERVAL         — seconds between verdict polls (default: 10)
#   GC_POLL_TIMEOUT          — max seconds to wait for verdict (default: 600)
#   GC_COMMIT_RANGE_TIMEOUT  — max seconds to wait for commit_range (default: 300)
#   GC_RENUDGE_INTERVAL      — seconds between re-nudges during poll (default: 60)
set -euo pipefail

BEAD_ID="${GC_BEAD_ID:?gate.sh requires GC_BEAD_ID}"
POLL_INTERVAL="${GC_POLL_INTERVAL:-10}"
POLL_TIMEOUT="${GC_POLL_TIMEOUT:-600}"
COMMIT_RANGE_TIMEOUT="${GC_COMMIT_RANGE_TIMEOUT:-300}"

# ---------------------------------------------------------------------------
# Step 1: Wait for commit_range metadata
#
# The provider's monitor process runs worker-collect.sh after the worker
# container exits. gc may invoke this gate before worker-collect finishes
# (race condition). Poll until timeout. (wx-kilk0)
# ---------------------------------------------------------------------------

commit_range=""
_cr_elapsed=0
while [[ "$_cr_elapsed" -lt "$COMMIT_RANGE_TIMEOUT" ]]; do
  # best-effort: bead metadata may not be written yet (polling)
  commit_range="$(bd show "$BEAD_ID" --json 2>/dev/null | jq -r '.[0].metadata.commit_range // empty' 2>/dev/null)" || commit_range=""
  [[ -n "$commit_range" ]] && break
  if (( _cr_elapsed > 0 && _cr_elapsed % 10 == 0 )); then
    echo "gate: waiting for commit_range on bead $BEAD_ID (${_cr_elapsed}s/${COMMIT_RANGE_TIMEOUT}s)" >&2
  fi
  sleep 2
  _cr_elapsed=$((_cr_elapsed + 2))
done

if [[ -z "$commit_range" ]]; then
  echo "gate: no commit_range set on bead $BEAD_ID after ${COMMIT_RANGE_TIMEOUT}s — worker may not have committed" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Step 2: Submit review request to the judge session
#
# submit (not nudge) so the runtime auto-wakes a session_sleep-suspended
# judge via ensureRunning; nudge is fenced out when the session is asleep.
# ---------------------------------------------------------------------------

REVIEW_MSG="Review bead $BEAD_ID — commit range: $commit_range"
RESUBMIT_INTERVAL="${GC_RENUDGE_INTERVAL:-60}"

gc session submit judge "$REVIEW_MSG"

# ---------------------------------------------------------------------------
# Step 3: Poll bead metadata for review_verdict
#
# Re-submit every RESUBMIT_INTERVAL seconds so a judge restart
# (which changes the epoch) doesn't fence out the request, and so a
# session that auto-suspended during the poll wakes back up.
# ---------------------------------------------------------------------------

elapsed=0
verdict=""
_since_submit=0

while [[ "$elapsed" -lt "$POLL_TIMEOUT" ]]; do
  # best-effort: verdict may not be set yet (polling)
  verdict="$(bd show "$BEAD_ID" --json 2>/dev/null | jq -r '.[0].metadata.review_verdict // empty' 2>/dev/null)" || verdict=""

  if [[ "$verdict" == "approve" ]] || [[ "$verdict" == "reject" ]]; then
    break
  fi

  if (( elapsed > 0 && elapsed % 60 == 0 )); then
    echo "gate: waiting for verdict on bead $BEAD_ID (${elapsed}s/${POLL_TIMEOUT}s)" >&2
  fi

  sleep "$POLL_INTERVAL"
  elapsed=$((elapsed + POLL_INTERVAL))
  _since_submit=$((_since_submit + POLL_INTERVAL))

  if (( _since_submit >= RESUBMIT_INTERVAL )); then
    echo "gate: re-submitting to judge for bead $BEAD_ID" >&2
    if ! gc session submit judge "$REVIEW_MSG"; then
      echo "gate: re-submit failed, will retry in ${RESUBMIT_INTERVAL}s" >&2
    fi
    _since_submit=0
  fi
done

# ---------------------------------------------------------------------------
# Step 4: Return exit code based on verdict
# ---------------------------------------------------------------------------

case "$verdict" in
  approve)
    echo "gate: bead $BEAD_ID approved by judge"
    exit 0
    ;;
  reject)
    echo "gate: bead $BEAD_ID rejected by judge"
    exit 1
    ;;
  *)
    echo "gate: timed out waiting for review verdict on bead $BEAD_ID (${POLL_TIMEOUT}s)" >&2
    exit 1
    ;;
esac
