#!/usr/bin/env bash
# Post-gate order — event-gated order triggered by convergence.terminated.
#
# Lightweight coordinator: notifies judge to merge (for approved convergences),
# handles deploy bead creation, escalation routing, and notifications.
# The judge owns the actual git operations (merge, rebase, cleanup).
#
# Exit codes:
#   0 — post-gate actions completed successfully
#   1 — error during post-gate processing
#
# Environment variables (set by gc order / city config):
#   GC_BEAD_ID          — bead that went through convergence (required)
#   GC_TERMINAL_REASON  — why convergence ended: "approved" or other (required)
#   GC_WORKSPACE        — host workspace path (required)
#   GC_CITY_NAME        — city name for notification context (required)
set -euo pipefail

BEAD_ID="${GC_BEAD_ID:?post-gate.sh requires GC_BEAD_ID}"
TERMINAL_REASON="${GC_TERMINAL_REASON:?post-gate.sh requires GC_TERMINAL_REASON}"
WORKSPACE="${GC_WORKSPACE:?post-gate.sh requires GC_WORKSPACE}"
CITY_NAME="${GC_CITY_NAME:?post-gate.sh requires GC_CITY_NAME}"

BRANCH="${BEAD_ID}"
WORKTREE_PATH=".wrapix/worktree/${BEAD_ID}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

notify() {
  wrapix-notify "Gas City" "$1" 2>/dev/null || true
}

# Clean up worktree and branch for a bead. Used by escalation path only —
# for approved convergences, the judge handles cleanup after merge.
cleanup_branch() {
  local branch="$1" worktree="$2"

  # Remove worktree directory directly — git worktree remove may fail because
  # provider.sh rewrites .git to a container-internal path (/mnt/git/...).
  if [[ -d "$worktree" ]]; then
    rm -rf "$worktree"
    git -C "$WORKSPACE" worktree prune 2>/dev/null || true
  fi

  if git -C "$WORKSPACE" rev-parse --verify "$branch" >/dev/null 2>&1; then
    git -C "$WORKSPACE" branch -d "$branch" 2>/dev/null || \
      git -C "$WORKSPACE" branch -D "$branch" 2>/dev/null || true
  fi
}

# Check if docs/orchestration.md has an Auto-deploy section.
has_auto_deploy() {
  local orch_file="${WORKSPACE}/docs/orchestration.md"
  [[ -f "$orch_file" ]] && grep -qE '^#+\s+Auto-deploy' "$orch_file"
}

# Check if the judge classified the change as low-risk.
is_low_risk() {
  # best-effort: missing risk_classification defaults to non-low-risk (safe)
  local risk
  risk="$(bd show "$BEAD_ID" --json 2>/dev/null | jq -r '.[0].metadata.risk_classification // empty' 2>/dev/null)" || risk=""
  [[ "$risk" == "low" ]]
}

# ---------------------------------------------------------------------------
# Escalation (terminal_reason != approved)
# ---------------------------------------------------------------------------

handle_escalation() {
  echo "post-gate: convergence escalated for bead $BEAD_ID (reason: $TERMINAL_REASON)"

  # Mark bead with escalation metadata so the mayor can find and present it.
  # These writes are critical — without them the escalation is invisible.
  bd update "$BEAD_ID" --set-metadata "escalated=true" ||
    echo "post-gate: ERROR: failed to set escalated=true on $BEAD_ID" >&2
  bd update "$BEAD_ID" --set-metadata "escalation_reason=$TERMINAL_REASON" ||
    echo "post-gate: ERROR: failed to set escalation_reason on $BEAD_ID" >&2
  bd update "$BEAD_ID" --notes="Convergence escalated: $TERMINAL_REASON — needs human review via mayor" ||
    echo "post-gate: ERROR: failed to update notes on $BEAD_ID" >&2

  # Flag for human review — mayor picks this up via bd human list.
  # Without this label the escalation is never surfaced.
  bd label add "$BEAD_ID" human ||
    echo "post-gate: ERROR: failed to add human label to $BEAD_ID" >&2

  # best-effort: notification channels — metadata above is authoritative
  gc mail send --to mayor -s "escalation" \
    -m "Convergence escalated for bead $BEAD_ID (reason: $TERMINAL_REASON). Worker→judge loop exhausted after max iterations. Review via bd show $BEAD_ID." \
    2>/dev/null || true

  # best-effort: desktop notification for when human is not attached
  notify "[${CITY_NAME}] Convergence escalated: bead ${BEAD_ID} — ${TERMINAL_REASON}"

  # Preserve worktree and branch for debugging — the mayor or human operator
  # needs the code state to investigate. Cleanup deferred to recovery.sh or
  # manual rm. (wx-kutwf)
  echo "post-gate: preserving worktree ${WORKTREE_PATH} and branch ${BRANCH} for inspection"
}

# ---------------------------------------------------------------------------
# Approved (terminal_reason == approved)
# ---------------------------------------------------------------------------

handle_approved() {
  echo "post-gate: convergence approved for bead $BEAD_ID"

  # Close the work bead — convergence succeeded, work is done. Without this,
  # the bead stays in_progress with gc.routed_to set, causing dispatch.sh to
  # count it as demand and the fallback bead picker to hand it to new workers.
  bd close "$BEAD_ID" ||
    echo "post-gate: ERROR: failed to close bead $BEAD_ID — may be re-dispatched" >&2

  # The judge's finalize step (judge-merge.sh approve) already merged and
  # cleaned up before gate.sh returned. Post-gate's job is the downstream
  # fan-out: deploy bead + notification.

  create_deploy_bead

  notify "[${CITY_NAME}] Convergence approved: bead ${BEAD_ID} — merged by judge"
}

# ---------------------------------------------------------------------------
# Deploy bead creation
# ---------------------------------------------------------------------------

create_deploy_bead() {
  local summary
  summary="$(bd show "$BEAD_ID" --json 2>/dev/null | head -1)" || summary=""

  local title
  title="$(echo "$summary" | grep -o '"title":"[^"]*"' | head -1 | cut -d'"' -f4)" || title="$BEAD_ID"

  local deploy_id
  deploy_id="$(bd create \
    --title="Deploy: ${title}" \
    --description="Deploy change from bead ${BEAD_ID}. Merged branch ${BEAD_ID} to main." \
    --type=task \
    --priority=2 \
    --labels="deploy,gc-deploy" \
    --silent)" || deploy_id=""

  if [[ -z "$deploy_id" ]]; then
    echo "post-gate: warning — failed to create deploy bead" >&2
    return 0
  fi

  echo "post-gate: created deploy bead $deploy_id"

  # Determine whether to flag for director approval or auto-deploy
  if has_auto_deploy && is_low_risk; then
    echo "post-gate: auto-deploy eligible (low-risk + Auto-deploy configured)"
    if ! bd update "$deploy_id" --set-metadata "auto_deploy=true"; then
      # Auto-deploy metadata failed — fall through to human approval as safe default
      echo "post-gate: WARNING: auto_deploy metadata failed, falling through to human approval" >&2
      # best-effort: auto_deploy path failed, human label is fallback safety net
      bd label add "$deploy_id" human 2>/dev/null || true
    fi
  else
    # Default: flag for director approval
    # best-effort: deploy bead exists but label failure is non-critical
    bd label add "$deploy_id" human 2>/dev/null || true
    notify "[${CITY_NAME}] Deploy approval needed: ${title} (bead ${deploy_id})"
  fi
}

# ---------------------------------------------------------------------------
# Main dispatch
# ---------------------------------------------------------------------------

case "$TERMINAL_REASON" in
  approved)
    handle_approved
    ;;
  *)
    handle_escalation
    ;;
esac
