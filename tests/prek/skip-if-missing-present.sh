#!/usr/bin/env bash
# Verifier for criterion 148 of specs/pre-commit.md:
#
#   `skip-if-missing <tool> -- <cmd>` execs `<cmd>` when `<tool>` resolves
#   on `PATH`.
#
# Drives the wrapper from its source file. See sibling pre-push-checks-*
# tests for rationale on the source-file approach.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-prek-skim-present.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

BASH_BIN="$(command -v bash)"
TOUCH_BIN="$(command -v touch)"

WRAPPER_BIN="$TEST_TMP/wrapper-bin/skip-if-missing"
mkdir -p "$(dirname "$WRAPPER_BIN")"
{
    printf '#!%s\n' "$BASH_BIN"
    cat "$REPO_ROOT/lib/prek/wrappers/skip-if-missing.sh"
} > "$WRAPPER_BIN"
chmod +x "$WRAPPER_BIN"

TOOL_STUB="$TEST_TMP/tool-bin/mytool"
mkdir -p "$(dirname "$TOOL_STUB")"
cat > "$TOOL_STUB" <<EOF
#!$BASH_BIN
exit 0
EOF
chmod +x "$TOOL_STUB"

SENTINEL="$TEST_TMP/sentinel"

rc=0
env -i PATH="$TEST_TMP/tool-bin:$TEST_TMP/wrapper-bin" \
    skip-if-missing mytool -- "$TOUCH_BIN" "$SENTINEL" || rc=$?

if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: wrapper exited $rc; expected 0 (touch should succeed)" >&2
    exit 1
fi

if [[ ! -e "$SENTINEL" ]]; then
    echo "FAIL: sentinel '$SENTINEL' was not created; expected wrapper to exec wrapped command" >&2
    exit 1
fi

echo "PASS: tool present → wrapper exec'd wrapped command, sentinel created"
