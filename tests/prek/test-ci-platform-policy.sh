#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-test-ci-policy.XXXXXX)"
FAKE_NIX_LOG="$TEST_TMP/fake-nix.log"
trap 'rm -rf "$TEST_TMP"' EXIT

mkdir -p "$TEST_TMP/bin"
cat >"$TEST_TMP/bin/nix" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"$WRIX_TEST_CI_FAKE_NIX_LOG"
exit "${WRIX_TEST_CI_FAKE_NIX_STATUS:-0}"
SCRIPT
chmod +x "$TEST_TMP/bin/nix"
cat >"$TEST_TMP/bin/uname" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$WRIX_TEST_CI_FAKE_PLATFORM"
SCRIPT
chmod +x "$TEST_TMP/bin/uname"

test_darwin_pre_push_runs_test_ci() {
  WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" \
    test-image-tier-graph \
    test-image-nix-config

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --json test-image-tier-graph test-image-nix-config" ]]; then
    echo "FAIL: Darwin pre-push did not run every selected test-ci target" >&2
    return 1
  fi
}

test_ci_failure_reaches_pre_push() {
  local status

  if WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_STATUS=19 \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" test-image-tier-graph; then
    status=0
  else
    status="$?"
  fi
  if [[ "$status" -ne 19 ]]; then
    echo "FAIL: Darwin pre-push suppressed test-ci failure $status; expected 19" >&2
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

test_darwin_pre_push_queries_inputs_without_execution_skip() {
  local output="$TEST_TMP/darwin-inputs.out"

  WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$TEST_TMP/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" --print-inputs test-image-tier-graph >"$output"

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --print-inputs test-image-tier-graph" ]]; then
    echo "FAIL: Darwin discovery did not invoke the input provider" >&2
    return 1
  fi
  if [[ -s "$output" ]]; then
    echo "FAIL: Darwin discovery was replaced by an execution skip verdict" >&2
    return 1
  fi
}

failed=0
if ! test_darwin_pre_push_runs_test_ci; then
  failed=$((failed + 1))
fi
if ! test_ci_failure_reaches_pre_push; then
  failed=$((failed + 1))
fi
if ! test_manual_darwin_keeps_test_ci; then
  failed=$((failed + 1))
fi
if ! test_linux_pre_push_keeps_test_ci; then
  failed=$((failed + 1))
fi

if ! test_darwin_pre_push_queries_inputs_without_execution_skip; then
  failed=$((failed + 1))
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

printf 'PASS: test-ci platform policy\n'
