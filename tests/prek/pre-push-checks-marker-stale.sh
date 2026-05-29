#!/usr/bin/env bash
# Verifier for criterion 142 of specs/pre-commit.md:
#
#   `pre-push-checks` execs the wrapped command when `.wrapix/loom/marker.json`
#   is present and `loom gate verify-marker` exits non-zero.
#
# Drives the wrapper from its source file. See marker-valid sibling for
# rationale on the source-file approach.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrapix-prek-marker-stale.XXXXXX)"
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

LOOM_SHIM="$TEST_TMP/loom-bin/loom"
mkdir -p "$(dirname "$LOOM_SHIM")"
cat > "$LOOM_SHIM" <<EOF
#!$BASH_BIN
set -euo pipefail
if [[ "\${1:-}" = "gate" && "\${2:-}" = "verify-marker" ]]; then
    exit 1
fi
echo "loom shim: unexpected args: \$*" >&2
exit 2
EOF
chmod +x "$LOOM_SHIM"

WORK="$TEST_TMP/work"
mkdir -p "$WORK/.wrapix/loom"
echo '{}' > "$WORK/.wrapix/loom/marker.json"

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
    echo "FAIL: sentinel '$SENTINEL' was not created; expected wrapper to fall through to exec" >&2
    exit 1
fi

echo "PASS: marker stale → wrapper exec'd wrapped command, sentinel created"
