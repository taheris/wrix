#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/security/audit-clock.sh
source "$SCRIPT_DIR/audit-clock.sh"
CLOCK_DATE="$1"
REAL_DATE="$2"
TEST_TMP=$(mktemp -d -t wrix-audit-clock.XXXXXX)
PIDS=()
cleanup() {
  local barrier pid status
  for barrier in "$TEST_TMP"/barrier-*; do
    if [[ -d "$barrier" ]]; then : >"$barrier/release"; fi
  done
  for pid in "${PIDS[@]}"; do
    if wait "$pid"; then
      :
    else
      status=$?
      printf 'audit clock self-test cleanup: child exited %s\n' "$status" >&2
    fi
  done
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

test_inactive_clock_preserves_arguments_and_errors() {
  local expected actual expected_status=0 actual_status=0

  expected=$("$REAL_DATE" -u --date=@0 '+%Y-%m-%d %H:%M:%S UTC')
  actual=$(WRIX_TEST_AUDIT_START_BARRIER="" "$CLOCK_DATE" -u --date=@0 '+%Y-%m-%d %H:%M:%S UTC')
  [[ "$actual" = "$expected" ]]
  "$REAL_DATE" --wrix-invalid-date-option >"$TEST_TMP/real.out" 2>"$TEST_TMP/real.err" || expected_status=$?
  WRIX_TEST_AUDIT_START_BARRIER="" "$CLOCK_DATE" --wrix-invalid-date-option \
    >"$TEST_TMP/clock.out" 2>"$TEST_TMP/clock.err" || actual_status=$?
  [[ "$expected_status" -ne 0 && "$actual_status" -eq "$expected_status" ]]
  cmp "$TEST_TMP/real.out" "$TEST_TMP/clock.out"
  cmp "$TEST_TMP/real.err" "$TEST_TMP/clock.err"
}

test_delayed_arrivals_record_the_real_same_second() {
  local barrier="$TEST_TMP/barrier-delayed" first_pid second_pid before after first_epoch second_epoch status=0

  mkdir -p "$barrier"
  before=$("$REAL_DATE" +%s)
  WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER=first \
    "$CLOCK_DATE" +%s >"$TEST_TMP/first-epoch" &
  first_pid=$!
  PIDS+=("$first_pid")
  (
    sleep 1.2
    WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER=second \
      "$CLOCK_DATE" +%s >"$TEST_TMP/second-epoch"
  ) &
  second_pid=$!
  PIDS+=("$second_pid")
  audit_wait_for_start_clocks "$barrier" "$first_pid" "$second_pid"
  [[ ! -s "$TEST_TMP/first-epoch" && ! -s "$TEST_TMP/second-epoch" ]]
  [[ $("$REAL_DATE" +%s) -gt "$before" ]]
  audit_release_start_clocks "$barrier"
  wait "$first_pid" || status=$?
  wait "$second_pid" || status=$?
  PIDS=()
  [[ "$status" -eq 0 ]]
  IFS= read -r first_epoch <"$TEST_TMP/first-epoch"
  IFS= read -r second_epoch <"$TEST_TMP/second-epoch"
  after=$("$REAL_DATE" +%s)
  [[ "$first_epoch" = "$second_epoch" && "$first_epoch" -ge "$before" && "$first_epoch" -le "$after" ]]
}

test_later_clock_reads_do_not_wait_again() {
  local barrier="$TEST_TMP/barrier-later" actual before after

  mkdir -p "$barrier/claimed-first"
  before=$("$REAL_DATE" +%s)
  actual=$(WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER=first "$CLOCK_DATE" +%s)
  after=$("$REAL_DATE" +%s)
  [[ "$actual" -ge "$before" && "$actual" -le "$after" && ! -e "$barrier/ready-first" ]]
}

test_non_start_formats_do_not_enter_the_barrier() {
  local barrier="$TEST_TMP/barrier-format" actual expected

  mkdir -p "$barrier"
  expected=$("$REAL_DATE" -u --date=@0 +%Y-%m-%dT%H:%M:%SZ)
  actual=$(WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER=first \
    "$CLOCK_DATE" -u --date=@0 +%Y-%m-%dT%H:%M:%SZ)
  [[ "$actual" = "$expected" && ! -e "$barrier/ready-first" ]]
}

test_invalid_member_is_rejected_without_ready_state() {
  local barrier="$TEST_TMP/barrier-invalid" status=0

  mkdir -p "$barrier"
  WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER=../escape \
    "$CLOCK_DATE" +%s >"$TEST_TMP/invalid.out" 2>"$TEST_TMP/invalid.err" || status=$?
  [[ "$status" -eq 64 && ! -s "$TEST_TMP/invalid.out" ]]
  [[ -z "$(find "$barrier" -mindepth 1 -print -quit)" ]]
  grep -q 'expected first or second member' "$TEST_TMP/invalid.err"
}

test_dead_launcher_is_a_readiness_failure() {
  local barrier="$TEST_TMP/barrier-dead" dead_pid status=0

  mkdir -p "$barrier"
  (exit 0) &
  dead_pid=$!
  wait "$dead_pid"
  audit_wait_for_start_clocks "$barrier" "$dead_pid" "$$" \
    >"$TEST_TMP/dead.out" 2>"$TEST_TMP/dead.err" || status=$?
  [[ "$status" -eq 1 ]]
  grep -q 'launcher exited before both start clocks were ready' "$TEST_TMP/dead.err"
}

test_overwritten_metadata_is_a_collision_failure() {
  local platform root output status

  for platform in linux darwin; do
    root="$TEST_TMP/mutation-$platform"
    output="$TEST_TMP/mutation-$platform.log"
    mkdir -p "$root"
    cp -R "${REPO_ROOT:?}/lib" "$REPO_ROOT/tests" "$root/"
    chmod -R u+w "$root"
    sed -i 's|log_file=$(mktemp --suffix=.json "/workspace/.wrix/log/${SESSION_START_ISO//\[:.\]/-}.XXXXXX")|log_file="/workspace/.wrix/log/${SESSION_START_ISO//[:.]/-}.json"|' \
      "$root/lib/sandbox/$platform/entrypoint.sh"
    status=0
    REPO_ROOT="$root" WRIX_TEST_AUDIT_CLOCK_DATE="$CLOCK_DATE" \
      bash "$root/tests/sandbox/entrypoint-contract.sh" test_same_second_audit_indexes_both_entrypoints \
      >"$output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]]
    grep -q "$platform did not retain two distinct same-second audit indexes" "$output"
  done
}

for test in \
  test_inactive_clock_preserves_arguments_and_errors \
  test_delayed_arrivals_record_the_real_same_second \
  test_later_clock_reads_do_not_wait_again \
  test_non_start_formats_do_not_enter_the_barrier \
  test_invalid_member_is_rejected_without_ready_state \
  test_dead_launcher_is_a_readiness_failure \
  test_overwritten_metadata_is_a_collision_failure; do
  "$test"
  printf 'PASS: %s\n' "$test"
done
