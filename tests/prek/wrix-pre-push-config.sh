#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

require_tool() {
  local tool="$1"

  command -v "$tool" >/dev/null 2>&1 || skip "$tool not on PATH"
}

require_tool bash
require_tool chmod
require_tool cp
require_tool git
require_tool mkdir
require_tool mktemp
require_tool prek
require_tool rm
require_tool skip-if-missing

BASH_BIN="$(command -v bash)"
PREK_BIN="$(command -v prek)"
TEST_TMP="$(mktemp -d -t wrix-pre-push-config.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

WORK="$TEST_TMP/work"
CALL_LOG="$TEST_TMP/loom-calls"
mkdir -p "$WORK/bin" "$WORK/.loom" "$TEST_TMP/bin"
cp "$REPO_ROOT/.pre-commit-config.yaml" "$WORK/.pre-commit-config.yaml"
cp "$REPO_ROOT/bin/pre-push-checks" "$WORK/bin/pre-push-checks"
git -C "$WORK" init -q
echo '{}' >"$WORK/.loom/marker.json"

cat >"$TEST_TMP/bin/loom" <<EOF
#!$BASH_BIN
set -euo pipefail
if [[ "\${1:-}" == "gate" && "\${2:-}" == "verify" ]]; then
  if [[ "\$#" -ne 4 || "\$3" != "--diff" || "\$4" != '@{upstream}..HEAD' ||
        "\${WRIX_PRE_PUSH:-}" != 1 || "\${RUSTC_WRAPPER-unset}" != "" ||
        "\${CARGO_BUILD_RUSTC_WRAPPER-unset}" != "" ]]; then
    echo "loom shim: incorrect verifier arguments or compiler wrappers" >&2
    exit 2
  fi
  printf 'verify\\n' >>"$CALL_LOG"
  exit "\${VERIFY_STATUS:-0}"
fi
if [[ "\$#" -ne 8 || "\${1:-}" != "gate" || "\${2:-}" != "verify-marker" ]]; then
  echo "loom shim: unexpected args: \$*" >&2
  exit 2
fi
printf '%s|%s|%s\n' "\$4" "\$6" "\$8" >>"$CALL_LOG"
EOF
chmod +x "$TEST_TMP/bin/loom"

(
  cd "$WORK"
  PATH="$TEST_TMP/bin:$PATH" PREK_COLOR=never \
    "$PREK_BIN" run --stage pre-push --all-files --no-progress
)

expected=$'nix-flake-check|nix flake check|@{u}..HEAD\nloom-gate-verify|skip-if-missing loom -- env WRIX_PRE_PUSH=1 RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= loom gate verify --diff @{upstream}..HEAD|@{u}..HEAD'
actual="$(<"$CALL_LOG")"
if [[ "$actual" != "$expected" ]]; then
  echo "FAIL: pre-push hooks did not pass canonical marker metadata" >&2
  echo "expected:" >&2
  printf '%s\n' "$expected" >&2
  echo "actual:" >&2
  printf '%s\n' "$actual" >&2
  exit 1
fi

printf 'PASS: pre-push hooks use bin/pre-push-checks with canonical metadata\n'

rm "$WORK/.loom/marker.json"
for verify_status in 0 19; do
  : >"$CALL_LOG"
  status=0
  (
    cd "$WORK"
    PATH="$TEST_TMP/bin:$PATH" PREK_COLOR=never VERIFY_STATUS="$verify_status" \
      RUSTC_WRAPPER=/unavailable/sccache CARGO_BUILD_RUSTC_WRAPPER=/unavailable/sccache \
      "$PREK_BIN" run loom-gate-verify --stage pre-push --all-files --no-progress
  ) || status=$?
  if [[ "$(<"$CALL_LOG")" != verify ]]; then
    echo "FAIL: pre-push did not execute the verifier without compiler wrappers" >&2
    exit 1
  fi
  if [[ "$verify_status" -eq 0 && "$status" -ne 0 ]] ||
    [[ "$verify_status" -ne 0 && "$status" -eq 0 ]]; then
    echo "FAIL: pre-push did not preserve verifier success/failure" >&2
    exit 1
  fi
done

printf 'PASS: Loom pre-push bypasses compiler caches without suppressing failures\n'
