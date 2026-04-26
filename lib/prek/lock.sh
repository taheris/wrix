#!/usr/bin/env bash
# Shared lock infrastructure for prek git hook shims.
#
# Sourced (not executed) by lib/prek/hooks/{pre-commit,pre-push}.
# On source: resolves _wrapix_dir and _lock_file from git-common-dir.
# Callers invoke _prek_acquire_lock explicitly when they need the lock.
#
# Lock semantics:
#   - flock on FD 9 against .wrapix/prek.lock (inode-level, not filename)
#   - FD 9 is inherited by exec'd processes (pre-commit → prek hook-impl)
#     so the lock is held for the full hook duration
#   - Callers that spawn subprocesses (not exec) should close FD 9 for
#     children (9>&-) to prevent orphans from holding the lock
#   - Dead-PID recovery: if the PID in the lock file is no longer running,
#     the lock file is deleted and re-acquired on a fresh inode
#   - 600s timeout with 1s poll; prints holder PID on wait and timeout
set -euo pipefail

if ! command -v flock >/dev/null; then
    echo "flock not found — enter nix develop or install util-linux" >&2
    exit 1
fi

_git_common_dir=$(git rev-parse --git-common-dir)
_wrapix_dir=$(dirname "$_git_common_dir")/.wrapix
_lock_file="$_wrapix_dir/prek.lock"

mkdir -p "$_wrapix_dir"

_prek_acquire_lock() {
    local waited=0 holder

    while true; do
        exec 9<>"$_lock_file"
        if flock --exclusive --nonblock 9 2>/dev/null; then
            printf '%d\n' $$ > "$_lock_file"
            return 0
        fi

        holder=$(<"$_lock_file") || holder=""
        if [[ -n "$holder" ]] && ! kill -0 "$holder" 2>/dev/null; then
            echo "prek: lock held by dead PID $holder — reclaiming" >&2
            exec 9>&-
            rm -f "$_lock_file"
            continue
        fi

        if [[ $waited -eq 0 ]]; then
            echo "prek: waiting for lock (held by PID ${holder:-<unknown>})..." >&2
        fi

        sleep 1
        waited=$((waited + 1))
        if [[ $waited -ge 600 ]]; then
            echo "prek: timed out after 600s; lock held by PID ${holder:-<unknown>}" >&2
            exit 1
        fi
    done
}
