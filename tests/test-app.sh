#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: tests/test-app.sh <test-name> [args...]" >&2
  exit 64
fi

name="$1"
shift

case "$name" in
  test-*) ;;
  *)
    echo "invalid test app name: $name" >&2
    exit 64
    ;;
esac

if ! command -v nix >/dev/null 2>&1; then
  echo "SKIP: nix not on PATH" >&2
  exit 77
fi

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

cd "$REPO_ROOT"
system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')
runner=$(nix build --no-link --print-out-paths --no-warn-dirty ".#legacyPackages.$system.testApps.$name")
exec "$runner/bin/$name" "$@"
