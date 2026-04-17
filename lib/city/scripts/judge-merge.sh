#!/usr/bin/env bash
# Judge finalize — writes review verdict, merges on approve, cleans up.
#
# Sole writer of review_verdict metadata. Called by the judge formula's
# finalize step in both approve and reject paths so the verdict, the
# merge, and the worktree/branch cleanup can never drift apart (wx-mc7k0).
#
# Usage:
#   judge-merge.sh approve
#   judge-merge.sh reject <reason>
#
# On approve: records review_verdict=approve, then attempts fast-forward
# merge. If main has advanced, rebases the branch onto main and re-runs
# prek before merging. On rebase conflicts or prek failures, converts the
# verdict to reject (see reject()) and exits 1 so convergence iterates.
#
# On reject: records review_verdict=reject and merge_failure=<reason>,
# reopens the bead. No merge attempted.
#
# EXIT trap always cleans up worktree and branch — guaranteed regardless
# of which exit path fires.
#
# Exit codes:
#   0 — approve merged, or reject recorded
#   1 — approve attempted but merge failed (verdict converted to reject)
#   2 — fatal error (missing env, bad args, bad state)
#
# Environment variables:
#   GC_BEAD_ID    — bead to finalize (required)
#   GC_WORKSPACE  — host workspace path (required)
set -euo pipefail

BEAD_ID="${GC_BEAD_ID:?judge-merge.sh requires GC_BEAD_ID}"
WORKSPACE="${GC_WORKSPACE:?judge-merge.sh requires GC_WORKSPACE}"

verdict="${1:-}"
case "$verdict" in
  approve)
    reject_reason=""
    ;;
  reject)
    reject_reason="${2:-}"
    if [[ -z "$reject_reason" ]]; then
      echo "judge-merge: 'reject' mode requires a reason string" >&2
      exit 2
    fi
    ;;
  "")
    echo "judge-merge: missing verdict arg (expected: approve | reject <reason>)" >&2
    exit 2
    ;;
  *)
    echo "judge-merge: unknown verdict '$verdict' (expected: approve | reject <reason>)" >&2
    exit 2
    ;;
esac

BRANCH="${BEAD_ID}"
WORKTREE="${WORKSPACE}/.wrapix/worktree/${BRANCH}"

# ---------------------------------------------------------------------------
# Cleanup — always runs
# ---------------------------------------------------------------------------

stashed=false

cleanup() {
  # Worktree is removed before merge attempt; guard handles edge cases
  if [[ -d "$WORKTREE" ]]; then
    rm -rf "$WORKTREE"
    # best-effort: bookkeeping after rm -rf already removed the directory
    git -C "$WORKSPACE" worktree prune 2>/dev/null || true
  fi
  # Checkout main BEFORE deleting the branch — git refuses to delete the
  # branch that HEAD points to, so the delete silently fails if we're
  # still on it (e.g. after rebase --abort leaves HEAD on the branch).
  if ! git -C "$WORKSPACE" checkout main 2>&1; then
    echo "judge-merge: WARNING: checkout main failed in cleanup" >&2
  fi
  if git -C "$WORKSPACE" rev-parse --verify "$BRANCH" >/dev/null 2>&1; then
    # Try soft delete, then force — branch may have unmerged commits on reject path
    git -C "$WORKSPACE" branch -d "$BRANCH" 2>/dev/null || \
      git -C "$WORKSPACE" branch -D "$BRANCH" 2>/dev/null || \
      echo "judge-merge: WARNING: could not delete branch $BRANCH" >&2
  fi
  if [[ "$stashed" == true ]]; then
    if ! git -C "$WORKSPACE" stash pop -q 2>&1; then
      echo "judge-merge: WARNING: stash pop failed — working tree may be dirty" >&2
    fi
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Reject helper — sets metadata and returns to open
# ---------------------------------------------------------------------------

reject() {
  local reason="$1"
  # These writes are critical — without them the bead stays in_progress
  # and the gate polls indefinitely or dispatch re-sends the same work.
  bd update "$BEAD_ID" --set-metadata "review_verdict=reject" ||
    echo "judge-merge: ERROR: failed to set review_verdict=reject on $BEAD_ID" >&2
  bd update "$BEAD_ID" --set-metadata "merge_failure=${reason}" ||
    echo "judge-merge: ERROR: failed to set merge_failure on $BEAD_ID" >&2
  bd update "$BEAD_ID" --status=open --notes="Judge rejected — ${reason}" ||
    echo "judge-merge: ERROR: failed to reopen $BEAD_ID" >&2
}

# ---------------------------------------------------------------------------
# Explicit reject — judge made a style-rule call; record and cleanup only
# ---------------------------------------------------------------------------

if [[ "$verdict" == "reject" ]]; then
  reject "$reject_reason"
  echo "judge-merge: recorded reject for $BEAD_ID — $reject_reason"
  exit 0
fi

# ---------------------------------------------------------------------------
# Approve — attempt merge first, then record verdict
#
# Verdict is written AFTER the merge succeeds so gate.sh, which polls
# review_verdict=approve, only observes approve once the branch has
# actually landed. This removes the race where downstream tests (and
# post-gate.sh) see approve before the merge commit exists.
# ---------------------------------------------------------------------------

# Free the branch from the worktree — git won't allow checkout of a branch
# that's checked out in a linked worktree. The worktree was the worker's
# scratch space; the branch tip has all the commits we need.
if [[ -d "$WORKTREE" ]]; then
  rm -rf "$WORKTREE"
  # best-effort: bookkeeping after rm -rf already removed the directory
  git -C "$WORKSPACE" worktree prune 2>/dev/null || true
fi

git -C "$WORKSPACE" checkout main

# Writes review_verdict=approve + approval note. Called from every merge
# success path so the metadata is never set until main has advanced.
record_approve() {
  bd update "$BEAD_ID" --set-metadata "review_verdict=approve" ||
    echo "judge-merge: ERROR: failed to set review_verdict=approve on $BEAD_ID" >&2
  bd update "$BEAD_ID" --notes="Judge approved — merged $BRANCH" ||
    echo "judge-merge: WARNING: failed to write approval note on $BEAD_ID" >&2
}

# Try fast-forward first (non-zero = not fast-forwardable, not an error)
if git -C "$WORKSPACE" merge --ff-only "$BRANCH" 2>/dev/null; then
  record_approve
  echo "judge-merge: fast-forward merged $BRANCH"
  exit 0
fi

# Main advanced — rebase branch onto main.
# Stash any dirty tracked files (e.g. city.toml modified by entrypoint)
# so git rebase doesn't fail on an unclean working tree.
if ! git -C "$WORKSPACE" diff --quiet 2>/dev/null; then
  git -C "$WORKSPACE" stash push -q && stashed=true
fi

git -C "$WORKSPACE" checkout "$BRANCH"

rebase_err=$(mktemp)
if ! git -C "$WORKSPACE" rebase main 2>"$rebase_err"; then
  conflict_details="$(cat "$rebase_err" 2>/dev/null || echo "rebase conflicts")"
  rm -f "$rebase_err"
  git -C "$WORKSPACE" rebase --abort 2>/dev/null || true
  reject "Rebase conflicts: ${conflict_details}"
  echo "judge-merge: rejected $BRANCH — rebase conflicts"
  exit 1
fi
rm -f "$rebase_err"

# Run prek if available (pre-commit checks after rebase)
if command -v prek >/dev/null 2>&1; then
  prek_out=$(mktemp)
  if ! (cd "$WORKSPACE" && prek run --stage pre-commit) >"$prek_out" 2>&1; then
    prek_details="$(cat "$prek_out" 2>/dev/null || echo "prek failure")"
    rm -f "$prek_out"
    git -C "$WORKSPACE" checkout main
    reject "Tests failed after rebase: ${prek_details}"
    echo "judge-merge: rejected $BRANCH — prek failed after rebase"
    exit 1
  fi
  rm -f "$prek_out"
fi

# Rebase succeeded, merge via fast-forward
git -C "$WORKSPACE" checkout main
if ! git -C "$WORKSPACE" merge --ff-only "$BRANCH" 2>/dev/null; then
  reject "Fast-forward failed after rebase (unexpected)"
  echo "judge-merge: rejected $BRANCH — post-rebase ff failed"
  exit 1
fi

record_approve
echo "judge-merge: rebased and merged $BRANCH"
exit 0
