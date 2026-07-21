#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-audit-trail.XXXXXX)
IMAGE_REFS=()
cleanup() {
  local image_ref

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

assert_audit_log_for_agent() {
  local agent="$1"
  local image_source image_ref profile_config spawn_config workspace out err rc log_count log_file field value session_dir host_session_dir binary host_marker

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

  for field in timestamp_start timestamp_end exit_code mode agent_session_dir; do
    value=$(jq -r ".${field} // empty" "$log_file")
    if [[ -z "$value" ]]; then
      fail "$agent: field $field empty/null in $log_file"
      sed 's/^/    /' "$log_file" >&2
      return
    fi
  done
  if jq -e 'has("claude_session_dir")' "$log_file" >/dev/null; then
    fail "$agent: deprecated claude_session_dir present in $log_file"
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

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
