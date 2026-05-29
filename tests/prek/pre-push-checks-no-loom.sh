#!/usr/bin/env bash
# Verifier for criterion 146 of specs/pre-commit.md:
#
#   `pre-push-checks` execs the wrapped command when `loom gate verify-marker`
#   is not on `PATH`.
#
# Resolution order: marker present but `loom` missing from PATH → exec.
# PATH is constrained to the wrapper bin dir only; no loom shim is wired,
# so `command -v loom` inside the wrapper returns failure. The wrapped
# command (touch) is invoked via absolute path so PATH does not need to
# carry coreutils.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrapix-prek-no-loom.XXXXXX)"
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

WORK="$TEST_TMP/work"
mkdir -p "$WORK/.wrapix/loom"
echo '{}' > "$WORK/.wrapix/loom/marker.json"

SENTINEL="$TEST_TMP/sentinel"

rc=0
(
    cd "$WORK"
    env -i PATH="$TEST_TMP/wrapper-bin" \
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

echo "PASS: loom missing → wrapper exec'd wrapped command despite marker present"
