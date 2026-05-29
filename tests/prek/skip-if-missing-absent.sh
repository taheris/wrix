#!/usr/bin/env bash
# Verifier for criterion 150 of specs/pre-commit.md:
#
#   `skip-if-missing <tool> -- <cmd>` exits 0 without running `<cmd>` when
#   `<tool>` is absent from `PATH`.
#
# Also asserts the wrapper produces no stderr output on the silent-skip
# path — the spec calls out "exit 0 silently". Drives the wrapper from
# its source file; see sibling tests for rationale.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrapix-prek-skim-absent.XXXXXX)"
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

SENTINEL="$TEST_TMP/sentinel"
STDERR_FILE="$TEST_TMP/stderr"

rc=0
env -i PATH="$TEST_TMP/wrapper-bin" \
    skip-if-missing mytool -- "$TOUCH_BIN" "$SENTINEL" \
    2>"$STDERR_FILE" || rc=$?

if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: wrapper exited $rc; expected 0" >&2
    if [[ -s "$STDERR_FILE" ]]; then
        echo "wrapper stderr was:" >&2
        cat "$STDERR_FILE" >&2
    fi
    exit 1
fi

if [[ -e "$SENTINEL" ]]; then
    echo "FAIL: sentinel '$SENTINEL' was created; expected wrapper to skip command" >&2
    exit 1
fi

if [[ -s "$STDERR_FILE" ]]; then
    echo "FAIL: wrapper wrote to stderr; expected silent skip. stderr was:" >&2
    cat "$STDERR_FILE" >&2
    exit 1
fi

echo "PASS: tool absent → wrapper exited 0 silently, sentinel not created"
