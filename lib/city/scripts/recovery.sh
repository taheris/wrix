#!/usr/bin/env bash
# Crash recovery — reconcile running containers and worktrees after gc restart.
#
# Called by entrypoint.sh before exec'ing gc start --foreground.
# Scans container runtime for running gc containers, reconciles against beads
# state, stops orphans, re-enters convergence for finished workers, and prunes
# stale worktrees.
#
# Environment variables (set by mkCity / systemd unit):
#   GC_CITY_NAME  — city name (required)
#   GC_WORKSPACE  — host workspace path (required)
set -euo pipefail

_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=container.sh
source "$_SCRIPT_DIR/container.sh"

CITY_NAME="${GC_CITY_NAME:?recovery.sh requires GC_CITY_NAME}"
WORKSPACE="${GC_WORKSPACE:?recovery.sh requires GC_WORKSPACE}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

list_city_container_names() {
  cr_ps_names_by_prefix "${CITY_NAME}-"
}

get_container_bead() {
  local container="$1"
  cr_label "$container" "gc-bead"
}

get_container_role() {
  local container="$1"
  cr_label "$container" "gc-role"
}

bead_in_progress() {
  local bead_id="$1"
  local status
  status="$(bd show "$bead_id" --json | jq -r '.[0].status // empty')" || status=""
  [[ "$status" == "in_progress" ]]
}

bead_is_open() {
  local bead_id="$1"
  local status
  status="$(bd show "$bead_id" --json | jq -r '.[0].status // empty')" || status=""
  [[ "$status" == "open" || "$status" == "in_progress" ]]
}

branch_has_commits() {
  local branch="$1"
  if ! git -C "$WORKSPACE" rev-parse --verify "$branch" >/dev/null 2>&1; then
    return 1
  fi
  local merge_base
  merge_base="$(git -C "$WORKSPACE" merge-base main "$branch")" || return 1
  local branch_head
  branch_head="$(git -C "$WORKSPACE" rev-parse "$branch")" || return 1
  [[ "$merge_base" != "$branch_head" ]]
}

stop_container() {
  local container="$1"
  echo "recovery: stopping orphaned container $container"
  cr_rm "$container"
}

# ---------------------------------------------------------------------------
# Step 1: Reconcile running worker containers against beads state
# ---------------------------------------------------------------------------

reconcile_workers() {
  local names
  names="$(list_city_container_names)" || names=""
  [[ -z "$names" ]] && return 0

  while IFS= read -r container; do
    [[ -z "$container" ]] && continue
    local role
    role="$(get_container_role "$container")"
    [[ "$role" == "worker" ]] || continue

    local bead_id
    bead_id="$(get_container_bead "$container")"

    if [[ -z "$bead_id" ]]; then
      stop_container "$container"
      continue
    fi

    if ! bead_in_progress "$bead_id"; then
      stop_container "$container"
      continue
    fi

    echo "recovery: worker container $container for bead $bead_id still in progress"
  done <<< "$names"
}

# ---------------------------------------------------------------------------
# Step 2: Find finished workers (commits on branch, bead still open)
# ---------------------------------------------------------------------------

reconcile_finished_workers() {
  local worktrees
  worktrees="$(find "${WORKSPACE}/.wrapix/worktree" -maxdepth 1 -mindepth 1 -type d 2>/dev/null)" || worktrees=""
  [[ -z "$worktrees" ]] && return 0

  while IFS= read -r worktree_path; do
    [[ -z "$worktree_path" ]] && continue

    local dir_name bead_id branch
    dir_name="$(basename "$worktree_path")"
    bead_id="$dir_name"
    branch="${bead_id}"

    if ! git -C "$WORKSPACE" rev-parse --verify "$branch" >/dev/null 2>&1; then
      continue
    fi

    if ! bead_is_open "$bead_id"; then
      continue
    fi

    local container_name="${CITY_NAME}-worker-${bead_id}"
    if cr_is_running "$container_name"; then
      continue
    fi

    if branch_has_commits "$branch"; then
      echo "recovery: finished worker for bead $bead_id — setting commit metadata for convergence re-entry"
      local merge_base
      merge_base="$(git -C "$WORKSPACE" merge-base main "$branch")" || merge_base=""
      if [[ -n "$merge_base" ]]; then
        bd update "$bead_id" --set-metadata "commit_range=${merge_base}..${branch}" ||
          echo "recovery: WARNING: failed to set commit_range on bead $bead_id" >&2
        bd update "$bead_id" --set-metadata "branch_name=$branch" ||
          echo "recovery: WARNING: failed to set branch_name on bead $bead_id" >&2
      fi
    fi
  done <<< "$worktrees"
}

# ---------------------------------------------------------------------------
# Step 3: Clean up stale worktrees
# ---------------------------------------------------------------------------

cleanup_stale_worktrees() {
  git -C "$WORKSPACE" worktree prune || true

  local worktrees
  worktrees="$(find "${WORKSPACE}/.wrapix/worktree" -maxdepth 1 -mindepth 1 -type d 2>/dev/null)" || worktrees=""
  [[ -z "$worktrees" ]] && return 0

  while IFS= read -r worktree_path; do
    [[ -z "$worktree_path" ]] && continue

    local dir_name bead_id
    dir_name="$(basename "$worktree_path")"
    bead_id="$dir_name"

    if bead_is_open "$bead_id"; then
      continue
    fi

    echo "recovery: removing stale worktree for bead $bead_id"
    rm -rf "$worktree_path"
    git -C "$WORKSPACE" worktree prune || true

    local branch="${bead_id}"
    if git -C "$WORKSPACE" rev-parse --verify "$branch" >/dev/null 2>&1; then
      git -C "$WORKSPACE" branch -d "$branch" || \
        git -C "$WORKSPACE" branch -D "$branch" || true
    fi
  done <<< "$worktrees"
}

# ---------------------------------------------------------------------------
# Step 4: Stop orphaned persistent containers (scout/judge) that gc
# will re-create on start
# ---------------------------------------------------------------------------

cleanup_persistent_containers() {
  local names
  names="$(list_city_container_names)" || names=""
  [[ -z "$names" ]] && return 0

  while IFS= read -r container; do
    [[ -z "$container" ]] && continue

    local role
    role="$(get_container_role "$container")"

    if [[ "$role" == "scout" || "$role" == "judge" ]]; then
      echo "recovery: stopping stale persistent container $container (gc will recreate)"
      cr_rm "$container"
    fi
  done <<< "$names"
}

# ---------------------------------------------------------------------------
# Step 5: Clean up stale tmux sockets
# ---------------------------------------------------------------------------

cleanup_stale_sockets() {
  local sock_dir="${WORKSPACE}/.wrapix/tmux"
  [[ -d "$sock_dir" ]] || return 0

  for sock in "$sock_dir"/*.sock; do
    [[ -e "$sock" ]] || continue
    local role
    role="$(basename "$sock" .sock)"
    local container="${CITY_NAME}-${role}"
    if ! cr_is_running "$container"; then
      echo "recovery: removing stale tmux socket for $role"
      rm -f "$sock"
    fi
  done
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

echo "recovery: scanning for containers from city ${CITY_NAME}..."

reconcile_workers
reconcile_finished_workers
cleanup_stale_worktrees
cleanup_persistent_containers
cleanup_stale_sockets

echo "recovery: reconciliation complete"
