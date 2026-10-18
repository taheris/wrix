#!/usr/bin/env bash
set -euo pipefail

# Cargo configuration stays local; locks and SQLite tracking follow the shared data.
wrix_link_tool_caches() {
    local root="$1" home="$2" name
    if [[ -d "$root/cargo" ]]; then
        mkdir -p "$home/.cargo"
        for name in registry git .package-cache .package-cache-mutate .global-cache; do
            if [[ -e "$home/.cargo/$name" || -L "$home/.cargo/$name" ]]; then
                printf 'wrix: managed cache destination already exists: %s\n' "$home/.cargo/$name" >&2
                return 1
            fi
            ln -s "$root/cargo/$name" "$home/.cargo/$name"
        done
    fi
    for name in uv sccache; do
        if [[ -d "$root/$name" ]]; then
            mkdir -p "$home/.cache"
            if [[ -e "$home/.cache/$name" || -L "$home/.cache/$name" ]]; then
                printf 'wrix: managed cache destination already exists: %s\n' "$home/.cache/$name" >&2
                return 1
            fi
            ln -s "$root/$name" "$home/.cache/$name"
        fi
    done
}
