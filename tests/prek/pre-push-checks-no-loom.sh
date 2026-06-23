#!/usr/bin/env bash
# Verifies pre-push-checks falls through when loom is absent from PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-no-loom.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool touch
TOUCH_BIN="$(command -v touch)"
PRE_PUSH_CHECKS_BIN="$(wrix_prek_wrapper_bin prePushChecks pre-push-checks)"
PRE_PUSH_CHECKS_DIR="$(dirname "$PRE_PUSH_CHECKS_BIN")"

WORK="$TEST_TMP/work"
mkdir -p "$WORK/.loom"
echo '{}' > "$WORK/.loom/marker.json"

SENTINEL="$TEST_TMP/sentinel"

rc=0
(
  cd "$WORK"
  env -i PATH="$PRE_PUSH_CHECKS_DIR" \
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
