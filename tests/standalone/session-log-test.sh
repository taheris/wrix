#!/usr/bin/env bash
# Unit tests for session transcript audit trail (.wrix/log/)
# Tests the write_session_log function from entrypoint scripts
# shellcheck disable=SC2329,SC2034  # SC2329: functions invoked via ALL_TESTS; SC2034: color vars used in functions
set -euo pipefail

PASSED=0
FAILED=0
FAILED_TESTS=()

# Colors
if [[ -t 2 ]]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[0;33m'
  CYAN='\033[0;36m'
  NC='\033[0m'
else
  RED='' GREEN='' YELLOW='' CYAN='' NC=''
fi

test_header() { echo -e "\n${CYAN}=== Test: $1 ===${NC}"; }
test_pass() { echo -e "  ${GREEN}PASS${NC}: $1"; ((PASSED++)) || true; }
test_fail() { echo -e "  ${RED}FAIL${NC}: $1"; ((FAILED++)) || true; FAILED_TESTS+=("$1"); }

#-----------------------------------------------------------------------------
# Setup / Teardown
#-----------------------------------------------------------------------------

setup() {
  TEST_DIR=$(mktemp -d -t "session-log-test-XXXXXX")
  export WORKSPACE="$TEST_DIR/workspace"
  mkdir -p "$WORKSPACE/.claude"
  mkdir -p "$WORKSPACE/.wrix"

  # Simulate the session start variables
  SESSION_START_EPOCH=$(date +%s)
  export SESSION_START_EPOCH
  SESSION_START_ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  export SESSION_START_ISO
}

teardown() {
  rm -rf "$TEST_DIR"
  unset LOOM_MODE WRIX_SESSION_ID
  rm -f /tmp/wrix-bead-id
}

# Portable write_session_log extracted from entrypoint (parameterized workspace)
write_session_log() {
  local exit_code="${1:-0}"
  local workspace="${2:-$WORKSPACE}"
  local end_epoch
  end_epoch=$(date +%s)
  local end_iso
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  local duration=$(( end_epoch - SESSION_START_EPOCH ))

  local mode="interactive"
  if [[ "${LOOM_MODE:-}" = "1" ]]; then
    mode="loom"
  fi

  local bead_id=""
  if [[ -f /tmp/wrix-bead-id ]]; then
    bead_id=$(cat /tmp/wrix-bead-id 2>/dev/null || true)
  fi

  local claude_session_id=""
  if [[ -f "$workspace/.claude/history.jsonl" ]]; then
    claude_session_id=$(tail -1 "$workspace/.claude/history.jsonl" 2>/dev/null \
      | jq -r '.sessionId // empty' 2>/dev/null || true)
  fi

  mkdir -p "$workspace/.wrix/log"
  local log_file="$workspace/.wrix/log/${SESSION_START_ISO//[:.]/-}.json"

  jq -n \
    --arg start "$SESSION_START_ISO" \
    --arg end "$end_iso" \
    --argjson duration "$duration" \
    --argjson exit_code "$exit_code" \
    --arg mode "$mode" \
    --arg bead_id "$bead_id" \
    --arg session_id "${WRIX_SESSION_ID:-}" \
    --arg claude_session_id "$claude_session_id" \
    --arg agent_session_dir "$workspace/.claude" \
    '{
      timestamp_start: $start,
      timestamp_end: $end,
      duration_seconds: $duration,
      exit_code: $exit_code,
      mode: $mode,
      bead_id: (if $bead_id == "" then null else $bead_id end),
      wrix_session_id: (if $session_id == "" then null else $session_id end),
      claude_session_id: (if $claude_session_id == "" then null else $claude_session_id end),
      agent_session_dir: $agent_session_dir
    }' > "$log_file" 2>/dev/null || true

  echo "$log_file"
}

#-----------------------------------------------------------------------------
# Tests
#-----------------------------------------------------------------------------

test_log_file_created() {
  test_header "Log file is created in .wrix/log/"
  setup

  local log_file
  log_file=$(write_session_log 0)

  if [[ -f "$log_file" ]]; then
    test_pass "Log file created at $log_file"
  else
    test_fail "Log file not created"
  fi

  teardown
}

test_log_valid_json() {
  test_header "Log entry is valid JSON"
  setup

  local log_file
  log_file=$(write_session_log 0)

  if jq empty "$log_file" 2>/dev/null; then
    test_pass "Log file is valid JSON"
  else
    test_fail "Log file is not valid JSON"
  fi

  teardown
}

test_log_required_fields() {
  test_header "Log entry contains all required fields"
  setup

  local log_file
  log_file=$(write_session_log 0)
  local json
  json=$(cat "$log_file")

  local fields=("timestamp_start" "timestamp_end" "duration_seconds" "exit_code" "mode" "bead_id" "agent_session_dir")
  for field in "${fields[@]}"; do
    if echo "$json" | jq -e "has(\"$field\")" >/dev/null 2>&1; then
      test_pass "Has field: $field"
    else
      test_fail "Missing field: $field"
    fi
  done

  teardown
}

test_log_deprecated_claude_session_dir_absent() {
  test_header "Deprecated claude_session_dir is absent"
  setup

  local log_file
  log_file=$(write_session_log 0)

  if jq -e 'has("claude_session_dir")' "$log_file" >/dev/null; then
    test_fail "Deprecated claude_session_dir is present"
  else
    test_pass "Deprecated claude_session_dir is absent"
  fi

  teardown
}

test_log_interactive_mode() {
  test_header "Interactive mode is recorded correctly"
  setup
  unset LOOM_MODE

  local log_file
  log_file=$(write_session_log 0)
  local mode
  mode=$(jq -r '.mode' "$log_file")

  if [[ "$mode" = "interactive" ]]; then
    test_pass "Mode is 'interactive'"
  else
    test_fail "Mode is '$mode', expected 'interactive'"
  fi

  teardown
}

test_log_loom_mode() {
  test_header "Loom mode is recorded correctly"
  setup
  export LOOM_MODE=1

  local log_file
  log_file=$(write_session_log 0)
  local mode
  mode=$(jq -r '.mode' "$log_file")

  if [[ "$mode" = "loom" ]]; then
    test_pass "Mode is 'loom'"
  else
    test_fail "Mode is '$mode', expected 'loom'"
  fi

  teardown
}

test_log_exit_code() {
  test_header "Exit code is recorded"
  setup

  local log_file
  log_file=$(write_session_log 42)
  local exit_code
  exit_code=$(jq '.exit_code' "$log_file")

  if [[ "$exit_code" = "42" ]]; then
    test_pass "Exit code recorded as 42"
  else
    test_fail "Exit code is '$exit_code', expected 42"
  fi

  teardown
}

test_log_bead_id_from_file() {
  test_header "Bead ID is read from /tmp/wrix-bead-id"
  setup
  echo "wx-abc123" > /tmp/wrix-bead-id

  local log_file
  log_file=$(write_session_log 0)
  local bead_id
  bead_id=$(jq -r '.bead_id' "$log_file")

  if [[ "$bead_id" = "wx-abc123" ]]; then
    test_pass "Bead ID recorded as wx-abc123"
  else
    test_fail "Bead ID is '$bead_id', expected 'wx-abc123'"
  fi

  teardown
}

test_log_bead_id_null_when_missing() {
  test_header "Bead ID is null when no file exists"
  setup
  rm -f /tmp/wrix-bead-id

  local log_file
  log_file=$(write_session_log 0)
  local bead_id
  bead_id=$(jq -r '.bead_id' "$log_file")

  if [[ "$bead_id" = "null" ]]; then
    test_pass "Bead ID is null"
  else
    test_fail "Bead ID is '$bead_id', expected null"
  fi

  teardown
}

test_log_claude_session_id() {
  test_header "Claude session ID is extracted from history.jsonl"
  setup

  # Create a mock history.jsonl
  echo '{"display":"test","sessionId":"abc-def-123"}' > "$WORKSPACE/.claude/history.jsonl"

  local log_file
  log_file=$(write_session_log 0)
  local session_id
  session_id=$(jq -r '.claude_session_id' "$log_file")

  if [[ "$session_id" = "abc-def-123" ]]; then
    test_pass "Claude session ID recorded as abc-def-123"
  else
    test_fail "Claude session ID is '$session_id', expected 'abc-def-123'"
  fi

  teardown
}

test_log_wrix_session_id() {
  test_header "WRIX_SESSION_ID is recorded"
  setup
  export WRIX_SESSION_ID="main:0.1"

  local log_file
  log_file=$(write_session_log 0)
  local session_id
  session_id=$(jq -r '.wrix_session_id' "$log_file")

  if [[ "$session_id" = "main:0.1" ]]; then
    test_pass "Wrix session ID recorded as main:0.1"
  else
    test_fail "Wrix session ID is '$session_id', expected 'main:0.1'"
  fi

  teardown
}

test_log_duration_non_negative() {
  test_header "Duration is non-negative"
  setup

  local log_file
  log_file=$(write_session_log 0)
  local duration
  duration=$(jq '.duration_seconds' "$log_file")

  if [[ "$duration" -ge 0 ]]; then
    test_pass "Duration is non-negative: $duration"
  else
    test_fail "Duration is negative: $duration"
  fi

  teardown
}

test_log_filename_format() {
  test_header "Log filename uses ISO 8601 derived format"
  setup

  local log_file
  log_file=$(write_session_log 0)
  local filename
  filename=$(basename "$log_file")

  # Should match pattern like 2026-02-08T10-30-00Z.json (colons and dots replaced with -)
  if [[ "$filename" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}Z\.json$ ]]; then
    test_pass "Filename matches expected format: $filename"
  else
    test_fail "Filename format unexpected: $filename"
  fi

  teardown
}

test_log_timestamps_iso8601() {
  test_header "Timestamps are ISO 8601 format"
  setup

  local log_file
  log_file=$(write_session_log 0)
  local start end_ts
  start=$(jq -r '.timestamp_start' "$log_file")
  end_ts=$(jq -r '.timestamp_end' "$log_file")

  # Check ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
  if [[ "$start" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]; then
    test_pass "Start timestamp is ISO 8601: $start"
  else
    test_fail "Start timestamp format unexpected: $start"
  fi

  if [[ "$end_ts" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]; then
    test_pass "End timestamp is ISO 8601: $end_ts"
  else
    test_fail "End timestamp format unexpected: $end_ts"
  fi

  teardown
}

#-----------------------------------------------------------------------------
# Run Tests
#-----------------------------------------------------------------------------

ALL_TESTS=(
  test_log_file_created
  test_log_valid_json
  test_log_required_fields
  test_log_deprecated_claude_session_dir_absent
  test_log_interactive_mode
  test_log_loom_mode
  test_log_exit_code
  test_log_bead_id_from_file
  test_log_bead_id_null_when_missing
  test_log_claude_session_id
  test_log_wrix_session_id
  test_log_duration_non_negative
  test_log_filename_format
  test_log_timestamps_iso8601
)

echo "Session Log Audit Trail Tests"
echo "=============================="

for test_fn in "${ALL_TESTS[@]}"; do
  "$test_fn"
done

echo ""
echo "=============================="
echo -e "Results: ${GREEN}$PASSED passed${NC}, ${RED}$FAILED failed${NC}"

if [[ ${#FAILED_TESTS[@]} -gt 0 ]]; then
  echo ""
  echo "Failed tests:"
  for ft in "${FAILED_TESTS[@]}"; do
    echo -e "  ${RED}✗${NC} $ft"
  done
  exit 1
fi

exit 0
