#!/usr/bin/env bash
# Verifier for criterion 144 of specs/pre-commit.md:
#
#   `pre-push-checks` execs the wrapped command when `.wrapix/loom/marker.json`
#   is absent.
#
# Resolution order step 1: marker absent → exec, regardless of whether the
# loom shim is reachable. The shim is on PATH but must not be invoked.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrapix-prek-no-marker.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

BASH_BIN="$(command -v bash)"
TOUCH_BIN="$(command -v touch)"

WRAPPER_BIN="$TEST_TMP/wrapper-bin/pre-push-checks"
mkdir -p "$(dirname "$WRAPPER_BIN")"
{
    printf '#!%s\n' "$BASH_BIN"
    cat "$REPO_ROOT/lib/prek/wrappers/pre-push-checks.sh"
} > "$WRAPPER_BIN"
chmod +x "$WRAPPER_BIN"

LOOM_CALL_LOG="$TEST_TMP/loom-calls"
LOOM_SHIM="$TEST_TMP/loom-bin/loom"
mkdir -p "$(dirname "$LOOM_SHIM")"
cat > "$LOOM_SHIM" <<EOF
#!$BASH_BIN
set -euo pipefail
echo "loom shim invoked with: \$*" >> "$LOOM_CALL_LOG"
exit 0
EOF
chmod +x "$LOOM_SHIM"

WORK="$TEST_TMP/work"
mkdir -p "$WORK"
# No marker file written.

SENTINEL="$TEST_TMP/sentinel"

rc=0
(
    cd "$WORK"
    PATH="$TEST_TMP/loom-bin:$TEST_TMP/wrapper-bin" \
        pre-push-checks "$TOUCH_BIN" "$SENTINEL"
) || rc=$?

if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: wrapper exited $rc; expected 0 (touch should succeed)" >&2
    exit 1
fi

if [[ ! -e "$SENTINEL" ]]; then
    echo "FAIL: sentinel '$SENTINEL' was not created; expected wrapper to exec wrapped command" >&2
    exit 1
fi

if [[ -e "$LOOM_CALL_LOG" ]]; then
    echo "FAIL: loom shim was invoked despite missing marker; calls: $(cat "$LOOM_CALL_LOG")" >&2
    exit 1
fi

echo "PASS: marker absent → wrapper exec'd wrapped command, loom never invoked"
