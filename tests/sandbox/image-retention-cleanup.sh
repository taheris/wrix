#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

if ! command -v nix >/dev/null 2>&1; then
  printf 'SKIP: nix is required for image-retention cleanup checks\n' >&2
  exit 77
fi

system="$(nix eval --raw --impure --expr builtins.currentSystem)"

nix build --no-warn-dirty \
  "$REPO_ROOT#checks.$system.remember-image-mru-bounds-and-identifiers" \
  "$REPO_ROOT#checks.$system.prune-stale-images-retention-policy"
