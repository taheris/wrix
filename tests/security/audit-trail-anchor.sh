#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox_linux
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

LAUNCHER=$(wrix_build_live_launcher)
DEPLOY_KEY="$TEST_TMP/deploy-key"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
PI_AUTH_FILE="$TEST_TMP/pi-auth.json"
mkdir -p "$HOME_DIR" "$XDG_CACHE_HOME"
wrix_make_ed25519_key "$DEPLOY_KEY" "audit-trail-test"
printf '{}\n' >"$PI_AUTH_FILE"
chmod 600 "$PI_AUTH_FILE"

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

transcript_marker() {
  local agent="$1"
  local session_dir

  session_dir=$(expected_session_dir "$agent")
  printf '%s\n' "$session_dir/wrix-audit-transcript-$agent"
}

write_agent_stub() {
  local agent="$1"
  local workspace="$2"
  local binary marker session_dir stub_dir stub_path

  binary=$(agent_binary "$agent")
  marker=$(transcript_marker "$agent")
  session_dir=$(expected_session_dir "$agent")
  stub_dir="$workspace/bin"
  stub_path="$stub_dir/$binary"
  mkdir -p "$stub_dir"
  cat >"$stub_path" <<EOF
#!/usr/bin/env bash
set -euo pipefail

probe_workspace="\${WRIX_AUDIT_PROBE_WORKSPACE:-/workspace}"
session_path="\$probe_workspace${session_dir#/workspace}"
marker_path="\$probe_workspace${marker#/workspace}"
mkdir -p "\$session_path"
{
  printf 'agent=%s\n' "$agent"
  printf 'argv='
  printf '%q ' "\$@"
  printf '\n'
} >"\$marker_path"
if [[ "$agent" = "claude" ]]; then
  printf '{"sessionId":"wrix-audit-%s"}\n' "$agent" >"\$probe_workspace/.claude/history.jsonl"
fi
EOF
  chmod +x "$stub_path"
}

assert_agent_probe_contract() {
  local agent="$1"
  local workspace="$TEST_TMP/probe-$agent"
  local binary marker host_marker out

  mkdir -p "$workspace"
  write_agent_stub "$agent" "$workspace"
  binary=$(agent_binary "$agent")
  marker=$(transcript_marker "$agent")
  host_marker="$workspace${marker#/workspace}"
  out="$TEST_TMP/probe-$agent.out"
  WRIX_AUDIT_PROBE_WORKSPACE="$workspace" "$workspace/bin/$binary" --probe >"$out"
  if [[ ! -f "$host_marker" ]]; then
    fail "$agent: agent probe self-test did not write marker: $host_marker"
    return
  fi
  if ! grep -qF "agent=$agent" "$host_marker"; then
    fail "$agent: agent probe self-test marker does not identify the agent"
    sed 's/^/    /' "$host_marker" >&2
    return
  fi
  pass "$agent: agent probe self-test writes its transcript marker"
}

assert_audit_log_for_agent() {
  local agent="$1"
  local image_source image_ref profile_config spawn_config workspace out err rc log_count log_file field value session_dir host_session_dir marker host_marker

  image_source=$(wrix_realize_test_image_source "$agent")
  image_ref=$(wrix_live_image_ref "audit-$agent-$$")
  IMAGE_REFS+=("$image_ref")
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

  marker=$(transcript_marker "$agent")
  host_marker="$workspace${marker#/workspace}"
  if [[ ! -f "$host_marker" ]]; then
    fail "$agent: selected agent did not write transcript marker: $host_marker"
    return
  fi
  if ! grep -qF "agent=$agent" "$host_marker"; then
    fail "$agent: transcript marker does not identify the selected agent"
    sed 's/^/    /' "$host_marker" >&2
    return
  fi

  pass "$agent: live launcher runs selected agent and writes mandatory session-metadata index"
}

assert_agent_probe_contract claude
assert_agent_probe_contract pi
assert_agent_probe_contract direct

assert_audit_log_for_agent claude
assert_audit_log_for_agent pi
assert_audit_log_for_agent direct

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
