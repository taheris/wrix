#!/usr/bin/env bash
# Verifies the wrappers run with only their declared ambient commands on PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-runtime-deps.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool bash
wrix_prek_require_tool git
wrix_prek_require_tool touch
BASH_BIN="$(command -v bash)"
GIT_DIR="$(dirname "$(command -v git)")"
TOUCH_BIN="$(command -v touch)"
PRE_PUSH_CHECKS_BIN="$(wrix_prek_wrapper_bin prePushChecks pre-push-checks)"
PRE_PUSH_CHECKS_DIR="$(dirname "$PRE_PUSH_CHECKS_BIN")"

WORK="$TEST_TMP/work"
LOOM_DIR="$TEST_TMP/loom-bin"
mkdir -p "$WORK/.loom" "$LOOM_DIR"
git -C "$WORK" init -q
printf 'opaque marker\n' >"$WORK/.loom/marker.json"
cat >"$LOOM_DIR/loom" <<EOF
#!$BASH_BIN
set -euo pipefail
exit 1
EOF
chmod +x "$LOOM_DIR/loom"

PRE_PUSH_SENTINEL="$TEST_TMP/pre-push-sentinel"
(
  cd "$WORK"
  env -i PATH="$LOOM_DIR:$PRE_PUSH_CHECKS_DIR:$GIT_DIR" \
    pre-push-checks --hook-id runtime-boundary \
    --hook-entry "$TOUCH_BIN $PRE_PUSH_SENTINEL" -- \
    "$TOUCH_BIN" "$PRE_PUSH_SENTINEL"
)
if [[ ! -e "$PRE_PUSH_SENTINEL" ]]; then
  echo "FAIL: pre-push-checks did not run with only Git and its wrapped command available" >&2
  exit 1
fi

printf 'PASS: pre-push-checks requires only its declared ambient commands\n'
