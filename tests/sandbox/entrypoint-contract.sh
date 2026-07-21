#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-entrypoint-contract.XXXXXX)"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'SKIP: %s not on PATH\n' "$command_name" >&2
    exit 77
  fi
}

entrypoint_source() {
  local platform="$1"
  case "$platform" in
    linux) printf '%s\n' "$REPO_ROOT/lib/sandbox/linux/entrypoint.sh" ;;
    darwin) printf '%s\n' "$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh" ;;
    *) fail "unknown entrypoint platform: $platform" ;;
  esac
}

agent_binary() {
  local agent="$1"
  case "$agent" in
    direct) printf '%s\n' "loom-direct-runner" ;;
    claude) printf '%s\n' "claude" ;;
    pi) printf '%s\n' "pi" ;;
    *) fail "unknown agent: $agent" ;;
  esac
}

write_fake_runtime_tools() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"

  cat >"$bin_dir/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "${WRIX_FAKE_GIT_LOG:-}" ]]; then
  printf '%s\n' "$*" >>"$WRIX_FAKE_GIT_LOG"
fi
exit 0
EOF
  chmod +x "$bin_dir/git"

  cat >"$bin_dir/sed" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 0
EOF
  chmod +x "$bin_dir/sed"

  cat >"$bin_dir/iptables" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
args=" $* "
if [[ "$args" == *" -S INPUT "* ]]; then
  printf '%s\n' '-P INPUT DROP'
  exit 0
fi
if [[ "$args" == *" -S OUTPUT "* ]]; then
  printf '%s\n' '-P OUTPUT DROP'
  exit 0
fi
exit 0
EOF
  chmod +x "$bin_dir/iptables"

  cat >"$bin_dir/ip6tables" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
args=" $* "
if [[ "$args" == *" -S OUTPUT "* ]]; then
  printf '%s\n' '-P OUTPUT DROP'
fi
exit 0
EOF
  chmod +x "$bin_dir/ip6tables"

  cat >"$bin_dir/capsh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" != "--drop=cap_net_admin" || "${2:-}" != "--" || "${3:-}" != "-c" ]]; then
  printf 'unexpected capsh invocation: %s\n' "$*" >&2
  exit 64
fi
script="$4"
shift 4
if [[ "$script" == *'-A OUTPUT -j ACCEPT'* || "$script" == *'add rule inet wrix output accept'* ]]; then
  exit 1
fi
exec bash -c "$script" "$@"
EOF
  chmod +x "$bin_dir/capsh"

  cat >"$bin_dir/getent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" != "ahostsv4" ]]; then
  exit 2
fi
printf '93.184.216.34 STREAM %s\n' "${2:-example.com}"
EOF
  chmod +x "$bin_dir/getent"

  cat >"$bin_dir/unshare" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
while [[ "$#" -gt 0 && "$1" != "--" ]]; do
  shift
done
if [[ "${1:-}" == "--" ]]; then
  shift
fi
exec "$@"
EOF
  chmod +x "$bin_dir/unshare"
}

rewrite_entrypoint() {
  local platform="$1"
  local workspace="$2"
  local etc_wrix="$3"
  local dest_path="$4"
  local home_dir="$5"
  local source_path setup_path capability_status ready_file capability_hex
  source_path="$(entrypoint_source "$platform")"
  setup_path="$workspace/git-ssh-setup.sh"
  capability_status="$workspace/proc-self-status"
  ready_file="$workspace/network-ready"
  capability_hex="${WRIX_TEST_CAP_STATUS_HEX:-0000000000000000}"
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' >"$setup_path"
  printf 'CapInh:\t%s\nCapPrm:\t%s\nCapEff:\t%s\nCapBnd:\t%s\nCapAmb:\t%s\n' \
    "$capability_hex" "$capability_hex" "$capability_hex" "$capability_hex" "$capability_hex" \
    >"$capability_status"
  : >"$ready_file"
  chmod +x "$setup_path"
  sed \
    -e "s|/workspace|$workspace|g" \
    -e "s|/home/wrix|$home_dir|g" \
    -e "s|/etc/wrix/|$etc_wrix/|g" \
    -e "s|\. /git-ssh-setup\.sh|. $setup_path|g" \
    -e "s|/proc/self/status|$capability_status|g" \
    -e "s|/run/wrix-network-ready|$ready_file|g" \
    "$source_path" >"$dest_path"
  chmod +x "$dest_path"
}

prepare_wrix_etc() {
  local etc_wrix="$1"
  local agent="$2"
  mkdir -p "$etc_wrix/pi-agent"
  printf '%s\n' "$agent" >"$etc_wrix/image-agent"
  printf '{}\n' >"$etc_wrix/claude-config.json"
  printf '{}\n' >"$etc_wrix/claude-settings.json"
  printf '{}\n' >"$etc_wrix/pi-agent/settings.json"
  if [[ "${WRIX_TEST_MCP_RUNTIME:-0}" == "1" ]]; then
    mkdir -p "$etc_wrix/mcp"
    if [[ -n "${WRIX_TEST_MCP_CONFIG:-}" ]]; then
      jq -e '.mcpServers.tmux' "$WRIX_TEST_MCP_CONFIG" >"$etc_wrix/mcp/tmux.json"
    else
      printf '%s\n' '{"command":"tmux-mcp","env":{}}' >"$etc_wrix/mcp/tmux.json"
    fi
    printf '%s\n' '{"command":"unselected-mcp"}' >"$etc_wrix/mcp/unselected.json"
  fi
}

run_entrypoint() {
  local platform="$1"
  local agent="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  local workspace="$5"
  shift 5
  local case_name case_dir tool_dir home_dir etc_wrix entrypoint
  case_name=$(basename "$stdout_path" .out)
  case_dir="$TEST_TMP/$platform-$agent-$case_name"
  tool_dir="$case_dir/tools"
  home_dir="$case_dir/home"
  etc_wrix="$case_dir/etc-wrix"
  entrypoint="$case_dir/entrypoint.sh"

  mkdir -p "$case_dir" "$home_dir" "$workspace/.claude" "$workspace/.wrix/log"
  write_fake_runtime_tools "$tool_dir"
  prepare_wrix_etc "$etc_wrix" "$agent"
  rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint" "$home_dir"

  env \
    HOME="$home_dir" \
    HOST_UID="$(id -u)" \
    PATH="$tool_dir:$PATH" \
    WRIX_AGENT="$agent" \
    WRIX_FIREWALL_BACKEND=iptables \
    WRIX_MCP="${WRIX_TEST_MCP_SELECTION:-}" \
    WRIX_MCP_TMUX_AUDIT="${WRIX_TEST_MCP_TMUX_AUDIT:-}" \
    WRIX_NETWORK=open \
    WRIX_STDIO=1 \
    bash "$entrypoint" "$@" >"$stdout_path" 2>"$stderr_path"
}

assert_output_contains() {
  local label="$1"
  local output="$2"
  local expected="$3"
  if [[ "$output" != *"$expected"* ]]; then
    fail "$label: missing '$expected' in output: $output"
  fi
}

test_workspace_bin_path_prepend_both() {
  require_command jq
  local platform
  for platform in linux darwin; do
    local workspace="$TEST_TMP/path-$platform/workspace"
    local stdout_path="$TEST_TMP/path-$platform.out"
    local stderr_path="$TEST_TMP/path-$platform.err"
    local output
    mkdir -p "$workspace/bin"
    cat >"$workspace/bin/path-probe" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'PATH_PROBE_RAN\n'
EOF
    chmod +x "$workspace/bin/path-probe"

    if ! run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
      bash -c 'printf "PATH=%s\n" "$PATH"; printf "PROBE=%s\n" "$(command -v path-probe)"; path-probe'; then
      fail "$platform entrypoint failed: $(<"$stderr_path")"
      return 1
    fi
    output="$(<"$stdout_path")"
    assert_output_contains "$platform PATH" "$output" "PATH=$workspace/bin:" || return 1
    assert_output_contains "$platform probe" "$output" "PROBE=$workspace/bin/path-probe" || return 1
    assert_output_contains "$platform shim" "$output" "PATH_PROBE_RAN" || return 1
  done
  printf 'PASS: both entrypoints prepend workspace/bin before command execution\n' >&2
}

test_agent_dispatch_both_entrypoints() {
  require_command jq
  local platform agent
  for platform in linux darwin; do
    for agent in direct claude pi; do
      local workspace="$TEST_TMP/agent-$platform-$agent/workspace"
      local stdout_path="$TEST_TMP/agent-$platform-$agent.out"
      local stderr_path="$TEST_TMP/agent-$platform-$agent.err"
      local binary output
      binary="$(agent_binary "$agent")"
      mkdir -p "$workspace/bin"
      cat >"$workspace/bin/$binary" <<EOF
#!/usr/bin/env bash
set -euo pipefail
printf 'AGENT_DISPATCH=%s\n' '$agent'
printf 'AGENT_ARGS=%s\n' "\$*"
EOF
      chmod +x "$workspace/bin/$binary"

      if ! run_entrypoint "$platform" "$agent" "$stdout_path" "$stderr_path" "$workspace"; then
        fail "$platform $agent entrypoint failed: $(<"$stderr_path")"
        return 1
      fi
      output="$(<"$stdout_path")"
      assert_output_contains "$platform $agent dispatch" "$output" "AGENT_DISPATCH=$agent" || return 1
      case "$agent" in
        claude) assert_output_contains "$platform claude args" "$output" "--input-format stream-json" || return 1 ;;
        pi) assert_output_contains "$platform pi args" "$output" "--mode rpc" || return 1 ;;
        direct) assert_output_contains "$platform direct args" "$output" "AGENT_ARGS=" || return 1 ;;
      esac
    done
  done
  printf 'PASS: both entrypoints dispatch WRIX_AGENT to direct, claude, and pi binaries\n' >&2
}

test_agent_config_homes_both_entrypoints() {
  require_command jq
  local platform
  for platform in linux darwin; do
    local claude_workspace="$TEST_TMP/config-$platform-claude/workspace"
    local claude_stdout="$TEST_TMP/config-$platform-claude.out"
    local claude_stderr="$TEST_TMP/config-$platform-claude.err"
    local claude_case claude_home
    claude_case="$TEST_TMP/$platform-claude-$(basename "$claude_stdout" .out)"
    claude_home="$claude_case/home"
    if ! run_entrypoint "$platform" claude "$claude_stdout" "$claude_stderr" "$claude_workspace" true; then
      fail "$platform claude config-home entrypoint failed: $(<"$claude_stderr")"
      return 1
    fi
    [[ -f "$claude_home/.claude.json" ]] || { fail "$platform claude config file missing"; return 1; }
    [[ -f "$claude_home/.claude/settings.json" ]] || { fail "$platform claude settings missing"; return 1; }
    [[ -f "$claude_workspace/.claude/settings.json" ]] || { fail "$platform workspace claude settings missing"; return 1; }

    local pi_workspace="$TEST_TMP/config-$platform-pi/workspace"
    local pi_stdout="$TEST_TMP/config-$platform-pi.out"
    local pi_stderr="$TEST_TMP/config-$platform-pi.err"
    local pi_case pi_home
    pi_case="$TEST_TMP/$platform-pi-$(basename "$pi_stdout" .out)"
    pi_home="$pi_case/home"
    if ! run_entrypoint "$platform" pi "$pi_stdout" "$pi_stderr" "$pi_workspace" true; then
      fail "$platform pi config-home entrypoint failed: $(<"$pi_stderr")"
      return 1
    fi
    [[ -f "$pi_home/.pi/agent/settings.json" ]] || { fail "$platform pi settings missing"; return 1; }
    [[ -d "$pi_workspace/.pi/agent/sessions" ]] || { fail "$platform pi sessions dir missing"; return 1; }
    [[ ! -e "$pi_home/.claude/settings.json" ]] || { fail "$platform pi run seeded claude settings"; return 1; }
  done
  printf 'PASS: both entrypoints seed claude and pi config homes separately\n' >&2
}

test_runtime_mcp_registration_uses_claude_user_config_both_entrypoints() {
  require_command jq
  local platform
  for platform in linux darwin; do
    local workspace="$TEST_TMP/runtime-mcp-$platform/workspace"
    local stdout_path="$TEST_TMP/runtime-mcp-$platform.out"
    local stderr_path="$TEST_TMP/runtime-mcp-$platform.err"
    local case_dir home_dir
    case_dir="$TEST_TMP/$platform-claude-$(basename "$stdout_path" .out)"
    home_dir="$case_dir/home"

    if ! WRIX_TEST_MCP_RUNTIME=1 \
      WRIX_TEST_MCP_SELECTION=tmux \
      WRIX_TEST_MCP_TMUX_AUDIT=/workspace/.debug-audit.log \
      run_entrypoint "$platform" claude "$stdout_path" "$stderr_path" "$workspace" true; then
      fail "$platform runtime MCP entrypoint failed: $(<"$stderr_path")"
      return 1
    fi
    if ! jq -e '
      .mcpServers.tmux.command == "tmux-mcp"
      and .mcpServers.tmux.env.TMUX_DEBUG_AUDIT == "/workspace/.debug-audit.log"
      and .mcpServers.unselected == null
    ' "$home_dir/.claude.json" >/dev/null; then
      fail "$platform runtime MCP registration missing from Claude user config"
      return 1
    fi
    if ! jq -e 'has("mcpServers") | not' "$home_dir/.claude/settings.json" >/dev/null; then
      fail "$platform runtime MCP registration leaked into Claude settings"
      return 1
    fi
  done
  printf 'PASS: both entrypoints register runtime MCP servers in Claude user config\n' >&2
}

test_runtime_mcp_registration_is_discovered_by_selected_claude() {
  require_command claude
  require_command jq
  require_command tmux
  require_command tmux-mcp
  local workspace="$TEST_TMP/runtime-mcp-live/workspace"
  local stdout_path="$TEST_TMP/runtime-mcp-live.out"
  local stderr_path="$TEST_TMP/runtime-mcp-live.err"
  local output

  if ! WRIX_TEST_MCP_RUNTIME=1 \
    WRIX_TEST_MCP_SELECTION=tmux \
    run_entrypoint linux claude "$stdout_path" "$stderr_path" "$workspace" \
    claude mcp get tmux; then
    fail "selected Claude runtime MCP health check failed: $(<"$stderr_path")"
    return 1
  fi
  output="$(<"$stdout_path")$(<"$stderr_path")"
  if [[ "$output" != *"tmux:"* || "$output" != *"Connected"* ]]; then
    fail "selected Claude did not connect to the runtime tmux MCP server: $output"
    return 1
  fi
  printf 'PASS: selected Claude discovers the runtime tmux MCP registration\n' >&2
}

run_core_hooks_path_case() {
  local platform="$1"
  local workspace="$TEST_TMP/hooks-$platform/workspace"
  local stdout_path="$TEST_TMP/hooks-$platform.out"
  local stderr_path="$TEST_TMP/hooks-$platform.err"
  local git_log="$TEST_TMP/hooks-$platform.git.log"
  local hooks_path="$TEST_TMP/hooks-$platform/prek-hooks"

  mkdir -p "$workspace/.git" "$hooks_path"
  printf 'repos: []\n' >"$workspace/.pre-commit-config.yaml"
  : >"$git_log"

  WRIX_PREK_HOOKS="$hooks_path"
  WRIX_FAKE_GIT_LOG="$git_log"
  export WRIX_PREK_HOOKS WRIX_FAKE_GIT_LOG
  if ! run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" true; then
    unset WRIX_PREK_HOOKS WRIX_FAKE_GIT_LOG
    fail "$platform entrypoint failed: $(<"$stderr_path")"
    return 1
  fi
  unset WRIX_PREK_HOOKS WRIX_FAKE_GIT_LOG

  if ! grep -qxF -- "-C $workspace config --local core.hooksPath $hooks_path" "$git_log"; then
    fail "$platform entrypoint did not configure core.hooksPath to WRIX_PREK_HOOKS; git log: $(<"$git_log")"
    return 1
  fi
}

test_linux_core_hooks_path() {
  require_command jq
  run_core_hooks_path_case linux
  printf 'PASS: linux entrypoint configures core.hooksPath when pre-commit config is present\n' >&2
}

test_darwin_core_hooks_path() {
  require_command jq
  run_core_hooks_path_case darwin
  printf 'PASS: darwin entrypoint configures core.hooksPath when pre-commit config is present\n' >&2
}

test_darwin_entrypoint_rejects_net_admin() {
  local workspace="$TEST_TMP/net-admin-darwin/workspace"
  local stdout_path="$TEST_TMP/net-admin-darwin.out"
  local stderr_path="$TEST_TMP/net-admin-darwin.err"
  mkdir -p "$workspace/bin"
  cat >"$workspace/bin/loom-direct-runner" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${WRIX_TEST_AGENT_RAN:?}"
EOF
  chmod +x "$workspace/bin/loom-direct-runner"

  export WRIX_TEST_CAP_STATUS_HEX=0000000000001000
  export WRIX_TEST_AGENT_RAN="$TEST_TMP/net-admin-darwin.agent-ran"
  if run_entrypoint darwin direct "$stdout_path" "$stderr_path" "$workspace"; then
    unset WRIX_TEST_CAP_STATUS_HEX WRIX_TEST_AGENT_RAN
    fail "Darwin entrypoint accepted NET_ADMIN after the bootstrap"
    return 1
  fi
  unset WRIX_TEST_CAP_STATUS_HEX WRIX_TEST_AGENT_RAN
  if [[ -e "$TEST_TMP/net-admin-darwin.agent-ran" ]]; then
    fail "Darwin entrypoint ran workspace code before rejecting NET_ADMIN"
    return 1
  fi
  grep -qF 'NET_ADMIN survived the Darwin network bootstrap' "$stderr_path" || {
    fail "Darwin entrypoint did not report the capability boundary failure: $(<"$stderr_path")"
    return 1
  }
  printf 'PASS: Darwin entrypoint rejects NET_ADMIN before workspace code runs\n' >&2
}

ALL_TESTS=(
  test_workspace_bin_path_prepend_both
  test_agent_dispatch_both_entrypoints
  test_agent_config_homes_both_entrypoints
  test_runtime_mcp_registration_uses_claude_user_config_both_entrypoints
  test_linux_core_hooks_path
  test_darwin_core_hooks_path
  test_darwin_entrypoint_rejects_net_admin
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  [[ "$failed" -eq 0 ]]
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
