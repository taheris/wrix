#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
# shellcheck source=tests/security/audit-clock.sh
source "$SCRIPT_DIR/audit-clock.sh"

cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-audit-trail.XXXXXX)
IMAGE_REFS=()
AUDIT_START_BARRIER=""
AUDIT_START_PIDS=()
cleanup() {
  local image_ref pid status

  if [[ -n "$AUDIT_START_BARRIER" && -d "$AUDIT_START_BARRIER" ]]; then
    : >"$AUDIT_START_BARRIER/release"
  fi
  for pid in "${AUDIT_START_PIDS[@]}"; do
    if wait "$pid"; then
      :
    else
      status=$?
      printf 'audit collision cleanup: launcher exited %s\n' "$status" >&2
    fi
  done
  rm -rf "$TEST_TMP"
  for image_ref in "${IMAGE_REFS[@]}"; do
    wrix_remove_image_ref "$image_ref"
  done
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() {
  local message="$1"
  printf '  PASS: %s\n' "$message"
  PASSED=$((PASSED + 1))
}

fail() {
  local message="$1"
  printf '  FAIL: %s\n' "$message" >&2
  FAILED=$((FAILED + 1))
}

LAUNCHER=""
DEPLOY_KEY="$TEST_TMP/deploy-key"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
PI_AUTH_FILE="$TEST_TMP/pi-auth.json"

expected_session_dir() {
  local agent="$1"

  case "$agent" in
    claude) printf '%s\n' "/workspace/.claude" ;;
    pi) printf '%s\n' "/workspace/.pi/agent/sessions" ;;
    direct) printf '%s\n' "/workspace" ;;
    *)
      printf 'unknown agent: %s\n' "$agent" >&2
      return 64
      ;;
  esac
}

agent_binary() {
  local agent="$1"

  case "$agent" in
    claude) printf '%s\n' "claude" ;;
    pi) printf '%s\n' "pi" ;;
    direct) printf '%s\n' "loom-direct-runner" ;;
    *)
      printf 'unknown agent: %s\n' "$agent" >&2
      return 64
      ;;
  esac
}

write_agent_stub() {
  local agent="$1"
  local workspace="$2"
  local binary stub_dir stub_path

  binary=$(agent_binary "$agent")
  stub_dir="$workspace/bin"
  stub_path="$stub_dir/$binary"
  mkdir -p "$stub_dir"
  cat >"$stub_path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

probe_workspace="${WRIX_AUDIT_PROBE_WORKSPACE:-/workspace}"
mkdir -p "$probe_workspace/.wrix"
jq -n \
  --arg agent "${WRIX_AGENT:-}" \
  --arg binary "$(basename "$0")" \
  '{agent: $agent, binary: $binary}' \
  >"$probe_workspace/.wrix/selected-agent.json"
EOF
  chmod +x "$stub_path"
}

assert_agent_probe_contract() {
  local agent="$1"
  local workspace="$TEST_TMP/probe-$agent"
  local binary host_marker

  mkdir -p "$workspace"
  write_agent_stub "$agent" "$workspace"
  binary=$(agent_binary "$agent")
  host_marker="$workspace/.wrix/selected-agent.json"
  WRIX_AGENT="$agent" WRIX_AUDIT_PROBE_WORKSPACE="$workspace" \
    "$workspace/bin/$binary" --probe
  if [[ ! -f "$host_marker" ]]; then
    fail "$agent: agent probe self-test did not write observation: $host_marker"
    return
  fi
  if ! jq -e --arg agent "$agent" --arg binary "$binary" \
    '.agent == $agent and .binary == $binary' "$host_marker" >/dev/null; then
    fail "$agent: agent probe self-test wrote an invalid observation"
    sed 's/^/    /' "$host_marker" >&2
    return
  fi
  pass "$agent: agent probe self-test records the invoked runtime"
}

assert_session_metadata_schema() {
  local agent="$1"
  local log_file="$2"

  if ! jq -e '
    type == "object"
    and has("timestamp_start")
    and has("timestamp_end")
    and has("duration_seconds")
    and has("exit_code")
    and has("mode")
    and has("bead_id")
    and has("wrix_session_id")
    and has("claude_session_id")
    and has("agent_session_dir")
    and (.timestamp_start | type == "string" and length > 0)
    and (.timestamp_end | type == "string" and length > 0)
    and (.duration_seconds | type == "number" and . >= 0 and floor == .)
    and (.exit_code | type == "number" and . >= 0 and floor == .)
    and (.mode == "interactive" or .mode == "loom")
    and (.bead_id == null or (.bead_id | type == "string" and length > 0))
    and (.wrix_session_id == null or (.wrix_session_id | type == "string" and length > 0))
    and (.claude_session_id == null or (.claude_session_id | type == "string" and length > 0))
    and (.agent_session_dir | type == "string" and length > 0)
  ' "$log_file" >/dev/null; then
    fail "$agent: session-metadata index has missing or invalid fields: $log_file"
    sed 's/^/    /' "$log_file" >&2
    return 1
  fi
  return 0
}

assert_audit_log_for_agent() {
  local agent="$1"
  local image_source image_ref profile_config spawn_config workspace out err rc log_count log_file value session_dir host_session_dir binary host_marker

  image_source=$(wrix_realize_test_image_source "$agent")
  image_ref=$(wrix_live_image_ref "audit-$agent-$$")
  IMAGE_REFS+=("$image_ref")
  wrix_remove_image_ref "$image_ref"
  profile_config="$TEST_TMP/profile-$agent.json"
  spawn_config="$TEST_TMP/spawn-$agent.json"
  workspace="$TEST_TMP/workspace-$agent"
  out="$TEST_TMP/$agent.out"
  err="$TEST_TMP/$agent.err"
  mkdir -p "$workspace"
  write_agent_stub "$agent" "$workspace"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" "$agent"
  wrix_write_spawn_config "$spawn_config" "$workspace"
  jq --arg bead_id "audit-$agent" \
    '.bead_id = $bead_id | .env += [["LOOM_MODE", "1"]]' \
    "$spawn_config" >"$spawn_config.tmp"
  mv "$spawn_config.tmp" "$spawn_config"

  rc=0
  if [[ "$agent" = "pi" ]]; then
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
      WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 WRIX_PI_AUTH_FILE="$PI_AUTH_FILE" \
      wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$out" 2>"$err" || rc=$?
  else
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
      WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
      wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$out" 2>"$err" || rc=$?
  fi
  if [[ "$rc" -ne 0 ]]; then
    fail "$agent: live launcher session failed"
    sed 's/^/    /' "$err" >&2
    return
  fi

  if [[ -d "$workspace/.wrix/log" ]]; then
    log_count=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' | wc -l)
  else
    log_count=0
  fi
  if [[ "$log_count" -ne 1 ]]; then
    fail "$agent: expected exactly one session-metadata JSON, got $log_count"
    return
  fi
  log_file=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' | head -n1)

  if ! assert_session_metadata_schema "$agent" "$log_file"; then
    return
  fi
  if jq -e 'has("claude_session_dir")' "$log_file" >/dev/null; then
    fail "$agent: deprecated claude_session_dir present in $log_file"
    sed 's/^/    /' "$log_file" >&2
    return
  fi
  if ! jq -e --arg bead_id "audit-$agent" \
    '.mode == "loom" and .bead_id == $bead_id' "$log_file" >/dev/null; then
    fail "$agent: mounted SpawnConfig bead_id did not reach the audit index"
    sed 's/^/    /' "$log_file" >&2
    return
  fi

  session_dir=$(expected_session_dir "$agent")
  value=$(jq -r '.agent_session_dir' "$log_file")
  if [[ "$value" != "$session_dir" ]]; then
    fail "$agent: unexpected agent_session_dir: $value"
    return
  fi
  host_session_dir="$workspace${session_dir#/workspace}"
  if [[ ! -d "$host_session_dir" ]]; then
    fail "$agent: agent_session_dir does not exist on host: $host_session_dir"
    return
  fi

  binary=$(agent_binary "$agent")
  host_marker="$workspace/.wrix/selected-agent.json"
  if [[ ! -f "$host_marker" ]]; then
    fail "$agent: selected agent did not write its runtime observation: $host_marker"
    return
  fi
  if ! jq -e --arg agent "$agent" --arg binary "$binary" \
    '.agent == $agent and .binary == $binary' "$host_marker" >/dev/null; then
    fail "$agent: selected-agent observation does not match ProfileConfig"
    sed 's/^/    /' "$host_marker" >&2
    return
  fi

  pass "$agent: live launcher runs selected agent and writes mandatory session-metadata index"
}

assert_same_second_sessions_have_distinct_indexes() {
  local agent="direct"
  local image_source image_ref profile_config spawn_config workspace warm_out warm_err
  local first_out first_err second_out second_err first_pid second_pid first_status second_status
  local first_config second_config member config ready_status=0 timestamp_count
  local -a log_files

  image_source=$(nix build --no-link --print-out-paths --no-warn-dirty \
    ".#${WRIX_TEST_AUDIT_COLLISION_IMAGE_ATTR:?run this verifier through test-ci}")
  image_ref=$(wrix_live_image_ref "audit-collision-$$")
  IMAGE_REFS+=("$image_ref")
  wrix_remove_image_ref "$image_ref"
  profile_config="$TEST_TMP/profile-collision.json"
  spawn_config="$TEST_TMP/spawn-collision.json"
  workspace="$TEST_TMP/workspace-collision"
  warm_out="$TEST_TMP/collision-warm.out"
  warm_err="$TEST_TMP/collision-warm.err"
  first_out="$TEST_TMP/collision-first.out"
  first_err="$TEST_TMP/collision-first.err"
  second_out="$TEST_TMP/collision-second.out"
  second_err="$TEST_TMP/collision-second.err"
  mkdir -p "$workspace"
  write_agent_stub "$agent" "$workspace"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" "$agent"
  wrix_write_spawn_config "$spawn_config" "$workspace"

  if ! HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$warm_out" 2>"$warm_err"; then
    fail "same-second audit setup launch failed"
    sed 's/^/    /' "$warm_err" >&2
    return
  fi

  rm -rf "$workspace/.wrix/log"
  AUDIT_START_BARRIER="$workspace/.wrix/audit-start"
  mkdir -p "$AUDIT_START_BARRIER"
  first_config="$TEST_TMP/spawn-collision-first.json"
  second_config="$TEST_TMP/spawn-collision-second.json"
  for member in first second; do
    config="$TEST_TMP/spawn-collision-$member.json"
    jq --arg member "$member" \
      '.env += [["WRIX_TEST_AUDIT_START_BARRIER", "/workspace/.wrix/audit-start"],
                ["WRIX_TEST_AUDIT_START_MEMBER", $member]]' "$spawn_config" >"$config"
  done

  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$first_config" >"$first_out" 2>"$first_err" &
  first_pid=$!
  AUDIT_START_PIDS+=("$first_pid")
  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$second_config" >"$second_out" 2>"$second_err" &
  second_pid=$!
  AUDIT_START_PIDS+=("$second_pid")
  audit_wait_for_start_clocks "$AUDIT_START_BARRIER" "$first_pid" "$second_pid" || ready_status=$?
  if [[ "$ready_status" -eq 0 ]]; then
    audit_release_start_clocks "$AUDIT_START_BARRIER"
  else
    : >"$AUDIT_START_BARRIER/release"
  fi
  first_status=0
  second_status=0
  wait "$first_pid" || first_status=$?
  wait "$second_pid" || second_status=$?
  AUDIT_START_PIDS=()
  if [[ "$ready_status" -ne 0 || "$first_status" -ne 0 || "$second_status" -ne 0 ]]; then
    fail "same-second audit startup failed (ready=$ready_status, first=$first_status, second=$second_status)"
    sed 's/^/    /' "$first_err" >&2
    sed 's/^/    /' "$second_err" >&2
    return
  fi

  mapfile -t log_files < <(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f | sort)
  if [[ "${#log_files[@]}" -lt 2 ]]; then
    fail "same-workspace sessions overwrote a session-metadata index"
    return
  fi
  if [[ "${#log_files[@]}" -gt 2 ]]; then
    fail "same-workspace sessions wrote more than one index each"
    return
  fi
  if ! assert_session_metadata_schema "same-second first" "${log_files[0]}" \
    || ! assert_session_metadata_schema "same-second second" "${log_files[1]}"; then
    return
  fi
  timestamp_count=$(jq -r '.timestamp_start' "${log_files[@]}" | sort -u | wc -l)
  if [[ "$timestamp_count" -ne 1 ]]; then
    fail "coordinated entrypoint start clocks did not record the same UTC second"
    return
  fi
  pass "same-workspace sessions starting in one second retain distinct metadata indexes"
}

assert_setup_failure_writes_audit_index() {
  local agent="direct"
  local image_source image_ref profile_config spawn_config workspace out err rc log_count log_file marker

  image_source=$(wrix_realize_test_image_source "$agent")
  image_ref=$(wrix_live_image_ref "audit-setup-failure-$$")
  IMAGE_REFS+=("$image_ref")
  wrix_remove_image_ref "$image_ref"
  profile_config="$TEST_TMP/profile-setup-failure.json"
  spawn_config="$TEST_TMP/spawn-setup-failure.json"
  workspace="$TEST_TMP/workspace-setup-failure"
  out="$TEST_TMP/setup-failure.out"
  err="$TEST_TMP/setup-failure.err"
  marker="$workspace/.wrix/selected-agent.json"
  mkdir -p "$workspace/.beads"
  write_agent_stub "$agent" "$workspace"
  printf 'sync-branch: beads\n' >"$workspace/.beads/config.yaml"
  printf '{"backend":"dolt"}\n' >"$workspace/.beads/metadata.json"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" "$agent"
  wrix_write_spawn_config "$spawn_config" "$workspace"

  rc=0
  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    fail "setup failure unexpectedly succeeded"
    return
  fi
  if [[ -f "$marker" ]]; then
    fail "setup failure reached the selected agent"
    return
  fi
  if [[ ! -d "$workspace/.wrix/log" ]]; then
    fail "setup failure did not create the audit-index directory"
    return
  fi
  log_count=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f | wc -l)
  if [[ "$log_count" -ne 1 ]]; then
    fail "setup failure wrote $log_count metadata indexes instead of one"
    return
  fi
  log_file=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f -print -quit)
  if ! assert_session_metadata_schema "setup failure" "$log_file"; then
    fail "setup failure did not write a valid session-metadata index"
    return
  fi
  if ! jq -e '.exit_code != 0' "$log_file" >/dev/null; then
    fail "setup failure audit index recorded a successful exit"
    return
  fi
  pass "setup failure writes one non-success session-metadata index"
}

assert_signaled_session_writes_audit_index() {
  local agent="direct"
  local image_source image_ref profile_config spawn_config workspace out err rc log_count log_file

  image_source=$(wrix_realize_test_image_source "$agent")
  image_ref=$(wrix_live_image_ref "audit-signal-$$")
  IMAGE_REFS+=("$image_ref")
  wrix_remove_image_ref "$image_ref"
  profile_config="$TEST_TMP/profile-signal.json"
  spawn_config="$TEST_TMP/spawn-signal.json"
  workspace="$TEST_TMP/workspace-signal"
  out="$TEST_TMP/signal.out"
  err="$TEST_TMP/signal.err"
  mkdir -p "$workspace"
  write_agent_stub "$agent" "$workspace"
  wrix_write_profile_config "$profile_config" "$image_ref" "$image_source" "$agent"
  wrix_write_spawn_config "$spawn_config" "$workspace" bash -lc 'kill -TERM 1; sleep 5'

  rc=0
  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    fail "signaled session unexpectedly succeeded"
    return
  fi
  if [[ ! -d "$workspace/.wrix/log" ]]; then
    fail "signaled session did not create the audit-index directory"
    return
  fi
  log_count=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f | wc -l)
  if [[ "$log_count" -ne 1 ]]; then
    fail "signaled session wrote $log_count metadata indexes instead of one"
    return
  fi
  log_file=$(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f -print -quit)
  if ! assert_session_metadata_schema "signaled session" "$log_file"; then
    return
  fi
  if ! jq -e '.exit_code == 143' "$log_file" >/dev/null; then
    fail "signaled session audit index did not record SIGTERM exit 143"
    sed 's/^/    /' "$log_file" >&2
    return
  fi
  pass "SIGTERM writes exactly one interrupted session-metadata index"
}

test_agent_probe_contracts() {
  assert_agent_probe_contract claude
  assert_agent_probe_contract pi
  assert_agent_probe_contract direct
}

if [[ "$#" -gt 0 ]]; then
  case "$1" in
    test_agent_probe_contracts) test_agent_probe_contracts ;;
    *) fail "unknown audit trail test function: $1" ;;
  esac
  [[ "$FAILED" -eq 0 ]]
  exit 0
fi

wrix_require_live_sandbox
LAUNCHER=$(wrix_build_live_launcher)
mkdir -p "$HOME_DIR" "$XDG_CACHE_HOME"
wrix_make_ed25519_key "$DEPLOY_KEY" "audit-trail-test"
printf '{}\n' >"$PI_AUTH_FILE"
chmod 600 "$PI_AUTH_FILE"

test_agent_probe_contracts
assert_audit_log_for_agent claude
assert_audit_log_for_agent pi
assert_audit_log_for_agent direct
assert_setup_failure_writes_audit_index
assert_signaled_session_writes_audit_index
assert_same_second_sessions_have_distinct_indexes

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
