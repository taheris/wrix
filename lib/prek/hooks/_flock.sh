#!/usr/bin/env bash
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

_prek_acquire_lock
