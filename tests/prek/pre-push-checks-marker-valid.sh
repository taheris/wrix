#!/usr/bin/env bash
# Verifies pre-push-checks short-circuits when loom validates marker.json.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-marker-valid.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool bash
wrix_prek_require_tool touch
BASH_BIN="$(command -v bash)"
TOUCH_BIN="$(command -v touch)"
PRE_PUSH_CHECKS_BIN="$(wrix_prek_wrapper_bin prePushChecks pre-push-checks)"
PRE_PUSH_CHECKS_DIR="$(dirname "$PRE_PUSH_CHECKS_BIN")"

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
mkdir -p "$WORK/.loom"
echo '{}' > "$WORK/.loom/marker.json"

SENTINEL="$TEST_TMP/sentinel"

rc=0
(
  cd "$WORK"
  PATH="$TEST_TMP/loom-bin:$PRE_PUSH_CHECKS_DIR" \
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
