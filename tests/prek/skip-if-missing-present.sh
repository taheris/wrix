#!/usr/bin/env bash
# Verifies skip-if-missing execs the command when the tool resolves on PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-skim-present.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool bash
wrix_prek_require_tool touch
BASH_BIN="$(command -v bash)"
TOUCH_BIN="$(command -v touch)"
SKIP_IF_MISSING_BIN="$(wrix_prek_wrapper_bin skipIfMissing skip-if-missing)"
SKIP_IF_MISSING_DIR="$(dirname "$SKIP_IF_MISSING_BIN")"

TOOL_STUB="$TEST_TMP/tool-bin/mytool"
mkdir -p "$(dirname "$TOOL_STUB")"
cat > "$TOOL_STUB" <<EOF
#!$BASH_BIN
set -euo pipefail
exit 0
EOF
chmod +x "$TOOL_STUB"

SENTINEL="$TEST_TMP/sentinel"

rc=0
env -i PATH="$TEST_TMP/tool-bin:$SKIP_IF_MISSING_DIR" \
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
