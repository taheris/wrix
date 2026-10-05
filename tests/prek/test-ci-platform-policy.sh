#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-test-ci-policy.XXXXXX)"
FAKE_NIX_LOG="$TEST_TMP/fake-nix.log"
trap 'rm -rf "$TEST_TMP"' EXIT

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

command -v jq >/dev/null 2>&1 || skip "jq not on PATH"

mkdir -p "$TEST_TMP/bin"
cat >"$TEST_TMP/bin/nix" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"$WRIX_TEST_CI_FAKE_NIX_LOG"
SCRIPT
chmod +x "$TEST_TMP/bin/nix"
cat >"$TEST_TMP/bin/uname" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$WRIX_TEST_CI_FAKE_PLATFORM"
SCRIPT
chmod +x "$TEST_TMP/bin/uname"

test_darwin_pre_push_skips_test_ci() {
  local output="$TEST_TMP/darwin-pre-push.jsonl" status

  if WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" \
    test-image-tier-graph \
    test-image-nix-config >"$output"; then
    status=0
  else
    status="$?"
  fi
  if [[ "$status" -ne 77 ]]; then
    echo "FAIL: Darwin policy skip exited $status; expected 77" >&2
    return 1
  fi

  if [[ -e "$FAKE_NIX_LOG" ]]; then
    echo "FAIL: Darwin pre-push invoked test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
  if [[ "$(jq -s 'length' "$output")" -ne 2 ]]; then
    echo "FAIL: Darwin pre-push did not emit one verdict per test-ci target" >&2
    return 1
  fi
  if ! jq -e -s 'all(.[]; .pass == false and .skipped == true and (.evidence | contains("disabled by default for Darwin pre-push")))' "$output" >/dev/null; then
    echo "FAIL: Darwin pre-push verdicts do not report the test-ci policy skip" >&2
    return 1
  fi
}

test_manual_darwin_keeps_test_ci() {
  WRIX_PRE_PUSH=0 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" test-image-tier-graph

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --json test-image-tier-graph" ]]; then
    echo "FAIL: manual Darwin verification did not retain test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
  rm -f "$FAKE_NIX_LOG"
}

test_linux_pre_push_keeps_test_ci() {
  WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Linux \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" test-image-tier-graph

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --json test-image-tier-graph" ]]; then
    echo "FAIL: Linux pre-push did not retain test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
}

failed=0
if ! test_darwin_pre_push_skips_test_ci; then
  failed=$((failed + 1))
fi
if ! test_manual_darwin_keeps_test_ci; then
  failed=$((failed + 1))
fi
if ! test_linux_pre_push_keeps_test_ci; then
  failed=$((failed + 1))
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

printf 'PASS: test-ci platform policy\n'
