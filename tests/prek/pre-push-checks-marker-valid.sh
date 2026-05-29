#!/usr/bin/env bash
# Verifier for criterion 140 of specs/pre-commit.md:
#
#   `pre-push-checks` exits 0 without running the wrapped command when
#   `.wrapix/loom/marker.json` is present and `loom gate verify-marker`
#   exits 0.
#
# Drives the wrapper from its source file in lib/prek/wrappers/. The
# wrapper's nix packaging (writeShellScriptBin) only adds an interpreter
# shebang and a $out/bin/ entry point; the resolution-order contract under
# test is encoded entirely in the .sh source. Synthesizing a shebang into
# a tmp copy exercises the same code path without requiring the host shell
# to have nix-built the wrapper onto PATH.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrapix-prek-marker-valid.XXXXXX)"
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
    exit 0
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
    echo "FAIL: wrapper exited $rc; expected 0" >&2
    exit 1
fi

if [[ -e "$SENTINEL" ]]; then
    echo "FAIL: sentinel '$SENTINEL' was created; expected short-circuit" >&2
    exit 1
fi

echo "PASS: marker valid → wrapper short-circuited, sentinel not created"
