#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$REPO_ROOT/tests/lib/live-sandbox.sh"

run_in_guest() {
    local test_script="$1"
    shift

    local current_system
    local sandbox
    local command_line
    local -a command

    case "$test_script" in
        screenshot-test.sh | smoke-test.sh) ;;
        *)
            printf 'unsupported Playwright guest test script: %s\n' "$test_script" >&2
            return 64
            ;;
    esac

    wrix_require_live_sandbox_darwin
    current_system=$(nix eval --raw --impure --expr builtins.currentSystem)
    sandbox=$(nix build --no-link --print-out-paths --no-warn-dirty \
        "$REPO_ROOT#legacyPackages.${current_system}.testApps.playwright-mcp-sandbox")
    command=(
        env WRIX_NETWORK=limit
        wrix --profile-config "$sandbox" run "$REPO_ROOT"
        /bin/bash "/workspace/tests/mcp/playwright/$test_script" "$@"
    )
    printf -v command_line '%q ' "${command[@]}"
    wrix_run_with_pty "$command_line"
}

if (($# == 0)); then
    printf 'usage: %s <screenshot-test.sh|smoke-test.sh> [test-function ...]\n' "$0" >&2
    exit 64
fi

run_in_guest "$@"
