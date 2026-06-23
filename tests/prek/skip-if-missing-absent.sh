#!/usr/bin/env bash
# Verifies skip-if-missing exits silently when the tool is absent from PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-skim-absent.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool touch
TOUCH_BIN="$(command -v touch)"
SKIP_IF_MISSING_BIN="$(wrix_prek_wrapper_bin skipIfMissing skip-if-missing)"
SKIP_IF_MISSING_DIR="$(dirname "$SKIP_IF_MISSING_BIN")"

SENTINEL="$TEST_TMP/sentinel"
STDERR_FILE="$TEST_TMP/stderr"

rc=0
env -i PATH="$SKIP_IF_MISSING_DIR" \
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
