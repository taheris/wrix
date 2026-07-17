#!/usr/bin/env bash
# Verifies pre-push-checks short-circuits when loom validates marker.json.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/prek/wrapper-test-lib.sh
source "$SCRIPT_DIR/wrapper-test-lib.sh"

TEST_TMP="$(mktemp -d -t wrix-prek-marker-valid.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

wrix_prek_require_tool bash
wrix_prek_require_tool git
wrix_prek_require_tool touch
BASH_BIN="$(command -v bash)"
GIT_DIR="$(dirname "$(command -v git)")"
TOUCH_BIN="$(command -v touch)"
PRE_PUSH_CHECKS_BIN="$(wrix_prek_wrapper_bin prePushChecks pre-push-checks)"
PRE_PUSH_CHECKS_DIR="$(dirname "$PRE_PUSH_CHECKS_BIN")"

LOOM_SHIM="$TEST_TMP/loom-bin/loom"
mkdir -p "$(dirname "$LOOM_SHIM")"
cat > "$LOOM_SHIM" <<EOF
#!$BASH_BIN
set -euo pipefail
if [[ "\${1:-}" = "gate" \
  && "\${2:-}" = "verify-marker" \
  && "\${3:-}" = "--hook-id" \
  && "\${4:-}" = "marker-valid" \
  && "\${5:-}" = "--hook-entry" \
  && "\${6:-}" = "$TOUCH_BIN $TEST_TMP/sentinel" \
  && "\${7:-}" = "--push-range" \
  && "\${8:-}" = "origin/main..HEAD" ]]; then
  exit 0
fi
echo "loom shim: unexpected args: \$*" >&2
exit 2
EOF
chmod +x "$LOOM_SHIM"

WORK="$TEST_TMP/work"
mkdir -p "$WORK/.loom"
git -C "$WORK" init -q -b main
git -C "$WORK" -c user.name=Test -c user.email=test@example.invalid \
  commit --allow-empty -qm initial
git -C "$WORK" remote add origin "$TEST_TMP/remote"
git -C "$WORK" update-ref refs/remotes/origin/main HEAD
git -C "$WORK" config branch.main.remote origin
git -C "$WORK" config branch.main.merge refs/heads/main
echo '{}' > "$WORK/.loom/marker.json"

SENTINEL="$TEST_TMP/sentinel"
HOOK_ENTRY="$TOUCH_BIN $SENTINEL"

rc=0
(
  cd "$WORK"
  PATH="$TEST_TMP/loom-bin:$PRE_PUSH_CHECKS_DIR:$GIT_DIR" \
    pre-push-checks --hook-id marker-valid --hook-entry "$HOOK_ENTRY" -- \
    "$TOUCH_BIN" "$SENTINEL"
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
