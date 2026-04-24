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

# Open read+write without truncating so a waiter can read the holder's PID
exec 9<>"$_lock_file"

if ! flock --exclusive --timeout 600 9; then
    holder=$(<"$_lock_file")
    if [[ -z "$holder" ]]; then
        holder="<unknown>"
    fi
    echo "prek.lock: timed out after 600s; lock held by PID $holder" >&2
    exit 1
fi

printf '%d\n' $$ > "$_lock_file"
