#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-entrypoint-contract.XXXXXX)"
unset WRIX_DIR_MOUNTS WRIX_FILE_MOUNTS
AUDIT_CLOCK_BARRIERS=()
AUDIT_CLOCK_PIDS=()

cleanup() {
  local barrier pid status
  for barrier in "${AUDIT_CLOCK_BARRIERS[@]}"; do
    : >"$barrier/release"
  done
  for pid in "${AUDIT_CLOCK_PIDS[@]}"; do
    if wait "$pid"; then
      :
    else
      status=$?
      printf 'entrypoint clock cleanup: child exited %s\n' "$status" >&2
    fi
  done
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
    direct) printf '%s\n' "consumer-agent" ;;
    claude) printf '%s\n' "claude" ;;
    pi) printf '%s\n' "pi" ;;
    *) fail "unknown agent: $agent" ;;
  esac
}

write_bash_fixture() {
  local path="$1"
  { printf '#!%s\n' "$BASH"; cat; } >"$path"
}

write_fake_runtime_tools() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"

  write_bash_fixture "$bin_dir/git" <<'EOF'
set -euo pipefail
if [[ -n "${WRIX_FAKE_GIT_LOG:-}" ]]; then
  printf '%s\n' "$*" >>"$WRIX_FAKE_GIT_LOG"
fi
exit 0
EOF
  chmod +x "$bin_dir/git"

  write_bash_fixture "$bin_dir/getent" <<'EOF'
set -euo pipefail
if [[ "${1:-}" != "ahostsv4" ]]; then
  exit 2
fi
printf '93.184.216.34 STREAM %s\n' "${2:-example.com}"
EOF
  chmod +x "$bin_dir/getent"

  write_bash_fixture "$bin_dir/bd" <<'EOF'
set -euo pipefail

log="${WRIX_FAKE_BD_LOG:?}"
state="${WRIX_FAKE_BD_STATE:?}"
printf '%s\n' "$*" >>"$log"
if [[ "$*" == '--readonly sql SELECT 1' ]]; then
  [[ "${BEADS_DOLT_AUTO_START:-}" == 0 && "${BD_IMPORT_AUTO:-}" == false ]]
  if [[ "${WRIX_FAKE_BD_UNREACHABLE:-0}" == 1 ]]; then
    echo 'Dolt server unreachable: connection refused' >&2
    exit 1
  fi
  exit 0
fi
if [[ "$*" == "dolt remote list" ]]; then
  if [[ -f "$state" ]]; then
    printf 'origin %s\n' "$(<"$state")"
  fi
  exit 0
fi
if [[ "${1:-}" == "sql" && "${2:-}" == "CALL DOLT_REMOTE('remove', 'origin')" ]]; then
  rm -f "$state"
  exit 0
fi
if [[ "${1:-}" == "sql" && "${2:-}" == "CALL DOLT_REMOTE('add', 'origin', "* ]]; then
  if [[ "$2" == *"${WRIX_EXPECTED_BD_REMOTE:?}"* ]]; then
    printf '%s\n' "$WRIX_EXPECTED_BD_REMOTE" >"$state"
  elif [[ "$2" == *"${WRIX_ORIGINAL_BD_REMOTE:?}"* ]]; then
    printf '%s\n' "$WRIX_ORIGINAL_BD_REMOTE" >"$state"
  else
    printf 'unexpected Dolt origin query: %s\n' "$2" >&2
    exit 64
  fi
  exit 0
fi
if [[ "${1:-}" == "dolt" && ( "${2:-}" == "pull" || "${2:-}" == "push" ) ]]; then
  printf '%s-origin=%s\n' "$2" "$(<"$state")" >>"$log"
  exit 0
fi
printf 'unexpected bd invocation: %s\n' "$*" >&2
exit 64
EOF
  chmod +x "$bin_dir/bd"

  write_bash_fixture "$bin_dir/unshare" <<'EOF'
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

  write_bash_fixture "$bin_dir/fixture-agent" <<'EOF'
set -euo pipefail
case "${WRIX_AGENT:-direct}" in
  pi) [[ "${1:-}" != --mode ]] || shift 2 ;;
  claude)
    if [[ "${1:-}" == --dangerously-skip-permissions ]]; then
      if [[ "${WRIX_STDIO:-}" == 1 ]]; then shift 7; else shift; fi
    fi
    ;;
esac
if [[ "$#" -gt 0 ]]; then
  exec "$@"
fi
EOF
  chmod +x "$bin_dir/fixture-agent"
  ln -sf fixture-agent "$bin_dir/pi"
  ln -sf fixture-agent "$bin_dir/claude"
}

test_runtime_fixtures_do_not_need_env_or_path() {
  local tools="$TEST_TMP/hermetic-tools"
  local log="$TEST_TMP/hermetic-bd.log"
  local state="$TEST_TMP/hermetic-bd.state"
  write_fake_runtime_tools "$tools"
  PATH="" "$tools/git" status || return "$?"
  PATH="" "$tools/getent" ahostsv4 example.invalid || return "$?"
  PATH="" WRIX_FAKE_BD_LOG="$log" WRIX_FAKE_BD_STATE="$state" \
    BEADS_DOLT_AUTO_START=0 BD_IMPORT_AUTO=false \
    "$tools/bd" --readonly sql 'SELECT 1' || return "$?"
  PATH="" "$tools/unshare" -- "$BASH" -c 'printf "PASS: hermetic runtime fixtures\\n"' || return "$?"
  local agent output status
  local -a flags
  for agent in direct pi claude; do
    flags=()
    case "$agent" in
      pi) flags=(--mode rpc) ;;
      claude) flags=(--dangerously-skip-permissions --print --verbose --input-format stream-json --output-format stream-json) ;;
    esac
    status=0
    # shellcheck disable=SC2016 # The fixture's child shell expands its argument.
    output=$(WRIX_AGENT="$agent" WRIX_STDIO=1 "$tools/fixture-agent" "${flags[@]}" \
      "$BASH" -c 'printf "fixture reply:%s" "$1"; exit 23' probe 'two words') || status=$?
    [[ "$status" == 23 && "$output" == 'fixture reply:two words' ]] || return 1
  done
}

rewrite_entrypoint() {
  local platform="$1"
  local workspace="$2"
  local etc_wrix="$3"
  local dest_path="$4"
  local home_dir="$5"
  local source_path setup_path mcp_helper capability_status ready_file capability_hex ready_helper field
  source_path="$(entrypoint_source "$platform")"
  setup_path="$workspace/git-ssh-setup.sh"
  mcp_helper="$workspace/mcp-manifest.sh"
  capability_status="$workspace/proc-self-status"
  ready_file="$workspace/network-ready"
  capability_hex="${WRIX_TEST_CAP_STATUS_HEX:-0000000000000000}"
  write_bash_fixture "$setup_path" <<< 'set -euo pipefail'
  for field in CapInh CapPrm CapEff CapBnd CapAmb; do
    if [[ "$field" == "${WRIX_TEST_CAP_FIELD:-CapEff}" ]]; then
      printf '%s:\t%s\n' "$field" "$capability_hex"
    else
      printf '%s:\t0000000000000000\n' "$field"
    fi
  done >"$capability_status"
  if [[ "${WRIX_TEST_NETWORK_READY:-1}" == 1 ]]; then
    : >"$ready_file"
  fi
  ready_helper="$workspace/network-ready.sh"
  sed \
    -e "s|/proc/self/status|$capability_status|g" \
    -e "s|/run/wrix-network-ready|$ready_file|g" \
    "$REPO_ROOT/lib/sandbox/network-ready.sh" >"$ready_helper"
  chmod +x "$setup_path"
  sed -e "s|/etc/wrix/|$etc_wrix/|g" "$REPO_ROOT/lib/sandbox/mcp-manifest.sh" >"$mcp_helper"
  sed \
    -e "s|/workspace|$workspace|g" \
    -e "s|/home/wrix|$home_dir|g" \
    -e "s|/etc/|$(dirname "$etc_wrix")/|g" \
    -e "s| -w /etc | -w $(dirname "$etc_wrix") |g" \
    -e "s|\. /git-ssh-setup\.sh|. $setup_path|g" \
    -e "s|\. /mcp-manifest\.sh|. $mcp_helper|g" \
    -e "s|\. /beads-sandbox\.sh|. $REPO_ROOT/lib/beads/sandbox.sh|g" \
    -e "s|\. /network-ready\.sh|. $ready_helper|g" \
    "$source_path" >"$dest_path"
  chmod +x "$dest_path"
}

prepare_wrix_etc() {
  local etc_wrix="$1"
  local agent="$2"
  mkdir -p "$etc_wrix/pi-agent" "$(dirname "$etc_wrix")/nix"
  printf 'wrix:x:1000:1000:Wrix Sandbox:/home/wrix:/bin/bash\n' >"$(dirname "$etc_wrix")/passwd"
  printf 'wrix:x:1000:\n' >"$(dirname "$etc_wrix")/group"
  : >"$(dirname "$etc_wrix")/nix/nix.conf"
  printf '%s\n' "${WRIX_TEST_IMAGE_AGENT:-$agent}" >"$etc_wrix/image-agent"
  printf '%s\n' "${WRIX_TEST_DIRECT_EXECUTABLE-$(dirname "$(dirname "$etc_wrix")")/tools/fixture-agent}" >"$etc_wrix/direct-executable"
  if [[ -n "${WRIX_TEST_DIRECT_METADATA:-}" ]]; then
    cp "$WRIX_TEST_DIRECT_METADATA" "$etc_wrix/direct-executable"
  fi
  printf '{}\n' >"$etc_wrix/claude-config.json"
  printf '{}\n' >"$etc_wrix/claude-settings.json"
  printf '{}\n' >"$etc_wrix/pi-agent/settings.json"
  if [[ "${WRIX_TEST_MCP_RUNTIME:-0}" == "1" ]]; then
    local tmux_config='{"command":"tmux-mcp","args":[],"env":{}}'
    local runtime_selection="${WRIX_TEST_MCP_RUNTIME_SELECTION:-true}"
    if [[ -n "${WRIX_TEST_MCP_CONFIG:-}" ]]; then
      tmux_config=$(jq -ec '
        if has("servers") then
          .servers[] | select(.name == "tmux") | del(.name)
        else
          .mcpServers.tmux
        end
      ' "$WRIX_TEST_MCP_CONFIG")
    fi
    jq -n \
      --argjson runtimeSelection "$runtime_selection" \
      --argjson tmux "$tmux_config" \
      '{
        schema: 1,
        runtime_selection: $runtimeSelection,
        servers:
          if $runtimeSelection then
            [
              ({ name: "tmux" } + $tmux),
              { name: "unselected", command: "unselected-mcp", args: [], env: {} }
            ]
          else
            [({ name: "tmux" } + $tmux)]
          end
      }' >"$etc_wrix/mcp-available.json"
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
  etc_wrix="$case_dir/etc/wrix"
  entrypoint="$case_dir/entrypoint.sh"

  mkdir -p "$case_dir" "$home_dir" "$workspace/.claude" "$workspace/.wrix/log"
  write_fake_runtime_tools "$tool_dir"
  prepare_wrix_etc "$etc_wrix" "$agent"
  if [[ "$agent" == direct && -x "$workspace/bin/consumer-agent" && ! -v WRIX_TEST_DIRECT_EXECUTABLE && -z "${WRIX_TEST_DIRECT_METADATA:-}" ]]; then
    printf '%s\n' "$workspace/bin/consumer-agent" >"$etc_wrix/direct-executable"
  fi
  if [[ "${WRIX_TEST_MISSING_DIRECT_METADATA:-0}" == 1 ]]; then
    rm "$etc_wrix/direct-executable"
  fi
  if [[ "${WRIX_TEST_MISSING_NATIVE_AGENT:-0}" == 1 || "${WRIX_TEST_REAL_NATIVE_AGENT:-0}" == 1 ]]; then
    rm "$tool_dir/$agent"
  fi
  rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint" "$home_dir"

  env \
    HOME="$home_dir" \
    HOST_UID="$(id -u)" \
    PATH="$tool_dir:${WRIX_TEST_RUNTIME_PATH-$PATH}" \
    WRIX_AGENT="${WRIX_TEST_SELECTED_AGENT-$agent}" \
    WRIX_FIREWALL_BACKEND=iptables \
    WRIX_MCP="${WRIX_TEST_MCP_SELECTION:-}" \
    WRIX_MCP_TMUX_AUDIT="${WRIX_TEST_MCP_TMUX_AUDIT:-}" \
    WRIX_MCP_TMUX_AUDIT_FULL="${WRIX_TEST_MCP_TMUX_AUDIT_FULL:-}" \
    WRIX_NETWORK=open \
    WRIX_STDIO="${WRIX_TEST_STDIO:-1}" \
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
    write_bash_fixture "$workspace/bin/path-probe" <<'EOF'
set -euo pipefail
printf 'PATH_PROBE_RAN\n'
EOF
    chmod +x "$workspace/bin/path-probe"

    # shellcheck disable=SC2016 # The entrypoint's inner shell expands these variables.
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

assert_consumer_reply() {
  local stdout_path="$1" stderr_path="$2" status="$3"
  [[ "$status" == 23 ]] || { fail "consumer exit status: $status"; return 1; }
  jq -e '. == ["two words", "", "$(exit 98)", "--help"]' <(head -1 "$stdout_path") >/dev/null || return 1
  [[ "$(tail -1 "$stdout_path")" == 'reply:request with spaces' ]] || return 1
  assert_output_contains 'consumer stderr' "$(<"$stderr_path")" 'consumer stderr'
}

test_consumer_package_stdio_contract() {
  local runner="${WRIX_TEST_CONSUMER_EXECUTABLE:?}" status=0
  local stdout_path="$TEST_TMP/consumer.out" stderr_path="$TEST_TMP/consumer.err"
  # shellcheck disable=SC2016 # Preserve the literal shell-looking agent argument.
  printf 'request with spaces\n' | "$runner" 'two words' '' '$(exit 98)' --help \
    >"$stdout_path" 2>"$stderr_path" || status=$?
  assert_consumer_reply "$stdout_path" "$stderr_path" "$status"
}

test_command_runner_package_contract() {
  local runner="${WRIX_TEST_COMMAND_RUNNER:?}" status=0
  local stdout_path="$TEST_TMP/command-runner.out" stderr_path="$TEST_TMP/command-runner.err"
  local workspace="$TEST_TMP/command-runner/workspace"
  mkdir -p "$workspace/bin"
  # shellcheck disable=SC2016 # Preserve the literal shell-looking agent argument.
  printf 'request with spaces\n' | "$runner" "${WRIX_TEST_CONSUMER_EXECUTABLE:?}" \
    'two words' '' '$(exit 98)' --help >"$stdout_path" 2>"$stderr_path" || status=$?
  assert_consumer_reply "$stdout_path" "$stderr_path" "$status" || return 1
  ln -s "$WRIX_TEST_CONSUMER_EXECUTABLE" "$workspace/bin/test-agent-probe"
  status=0
  printf 'request with spaces\n' | WRIX_TEST_PROBE_WORKSPACE="$workspace" "$runner" \
    >"$stdout_path" 2>"$stderr_path" || status=$?
  [[ "$status" == 23 ]] || return 1
  jq -e '. == []' <(head -1 "$stdout_path") >/dev/null || return 1
  [[ "$(tail -1 "$stdout_path")" == 'reply:request with spaces' ]] || return 1
  assert_output_contains 'command runner stderr' "$(<"$stderr_path")" 'consumer stderr'
}

test_declared_direct_runner_preserves_argv_stdio_both() {
  require_command jq
  local platform stdio selected status workspace stdout_path stderr_path
  for platform in linux darwin; do
    for stdio in 0 1; do
      workspace="$TEST_TMP/declared-$platform-$stdio/workspace"
      stdout_path="$TEST_TMP/declared-$platform-$stdio.out"
      stderr_path="$TEST_TMP/declared-$platform-$stdio.err"
      mkdir -p "$workspace/bin"
      write_bash_fixture "$workspace/bin/consumer-agent" <<< $'set -euo pipefail\nexit 98'
      chmod +x "$workspace/bin/consumer-agent"
      status=0
      selected=direct
      if [[ "$stdio" == 0 ]]; then selected=""; fi
      # shellcheck disable=SC2016 # Preserve the literal shell-looking agent argument.
      printf 'request with spaces\n' | WRIX_TEST_STDIO="$stdio" WRIX_TEST_SELECTED_AGENT="$selected" \
        WRIX_AGENT_BIN="$workspace/bin/consumer-agent" DIRECT_EXECUTABLE_FILE="$workspace/bin/consumer-agent" \
        WRIX_TEST_DIRECT_METADATA="${WRIX_TEST_CONSUMER_METADATA:?}" \
        run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
        'two words' '' '$(exit 98)' --help || status=$?
      assert_consumer_reply "$stdout_path" "$stderr_path" "$status" || return 1
      jq -e '.exit_code == 23' "$workspace/.wrix/log/"*.json >/dev/null || return 1
    done
  done
  printf 'PASS: both entrypoints execute the declared package path, not PATH/env overrides, preserving argv/stdio/status\n' >&2
}

test_direct_executable_guard_both_entrypoints() {
  local platform failure workspace stdout_path stderr_path executable
  for platform in linux darwin; do
    for failure in absent nonexecutable relative empty; do
      workspace="$TEST_TMP/missing-$platform-$failure/workspace"
      stdout_path="$TEST_TMP/missing-$platform-$failure.out"
      stderr_path="$TEST_TMP/missing-$platform-$failure.err"
      mkdir -p "$workspace"
      executable="$workspace/missing-consumer"
      case "$failure" in
        nonexecutable) printf 'not executable\n' >"$executable" ;;
        relative) executable=fixture-agent ;;
        empty) executable="" ;;
      esac
      if WRIX_TEST_DIRECT_EXECUTABLE="$executable" \
        run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
        "$BASH" -c 'printf AGENT_RAN'; then
        fail "$platform accepted a $failure direct executable"
        return 1
      fi
      [[ ! -s "$stdout_path" ]] || { fail "$platform bypassed the guard with argv"; return 1; }
      assert_output_contains "$platform executable diagnostic" "$(<"$stderr_path")" "$executable" || return 1
      assert_output_contains "$platform executable diagnostic" "$(<"$stderr_path")" 'not present or executable' || return 1
    done
  done
}

test_direct_metadata_missing_blocks_agent_both_entrypoints() {
  local platform workspace stdout_path stderr_path
  for platform in linux darwin; do
    workspace="$TEST_TMP/missing-metadata-$platform/workspace"
    stdout_path="$TEST_TMP/missing-metadata-$platform.out"
    stderr_path="$TEST_TMP/missing-metadata-$platform.err"
    if WRIX_TEST_MISSING_DIRECT_METADATA=1 \
      run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
      "$BASH" -c 'printf AGENT_RAN'; then
      fail "$platform accepted missing direct metadata"
      return 1
    fi
    [[ ! -s "$stdout_path" ]] || return 1
    assert_output_contains "$platform missing metadata" "$(<"$stderr_path")" 'direct executable metadata is missing' || return 1
  done
}

test_native_executable_missing_blocks_agent_both_entrypoints() {
  local tools="$TEST_TMP/guard-runtime" platform agent tool workspace stdout_path stderr_path
  mkdir -p "$tools"
  for tool in bash date mkdir jq mktemp grep sed chmod mv; do
    ln -s "$(command -v "$tool")" "$tools/$tool"
  done
  for platform in linux darwin; do
    for agent in pi claude; do
      workspace="$TEST_TMP/missing-native-$platform-$agent/workspace"
      stdout_path="$TEST_TMP/missing-native-$platform-$agent.out"
      stderr_path="$TEST_TMP/missing-native-$platform-$agent.err"
      if WRIX_TEST_MISSING_NATIVE_AGENT=1 WRIX_TEST_RUNTIME_PATH="$tools" \
        run_entrypoint "$platform" "$agent" "$stdout_path" "$stderr_path" "$workspace" \
        "$BASH" -c 'printf AGENT_RAN'; then
        fail "$platform accepted missing $agent"
        return 1
      fi
      [[ ! -s "$stdout_path" ]] || return 1
      assert_output_contains "$platform missing $agent" "$(<"$stderr_path")" "WRIX_AGENT=$agent selects '$agent', but that binary is not present" || return 1
    done
  done
}

test_immutable_agent_variant_guard_both_entrypoints() {
  local platform image_agent agent workspace stdout_path stderr_path
  for platform in linux darwin; do
    for image_agent in direct pi claude; do
      for agent in direct pi claude; do
        [[ "$agent" != "$image_agent" ]] || continue
        workspace="$TEST_TMP/variant-$platform-$image_agent-$agent/workspace"
        stdout_path="$TEST_TMP/variant-$platform-$image_agent-$agent.out"
        stderr_path="$TEST_TMP/variant-$platform-$image_agent-$agent.err"
        if WRIX_TEST_IMAGE_AGENT="$image_agent" WRIX_TEST_SELECTED_AGENT="$agent" \
          WRIX_TEST_DIRECT_EXECUTABLE="$workspace/missing-consumer" \
          run_entrypoint "$platform" "$agent" "$stdout_path" "$stderr_path" "$workspace" \
          "$BASH" -c 'printf AGENT_RAN'; then
          fail "$platform accepted an immutable agent mismatch"
          return 1
        fi
        [[ ! -s "$stdout_path" ]] || return 1
        assert_output_contains "$platform mismatch" "$(<"$stderr_path")" "ProfileConfig selected WRIX_AGENT=$agent" || return 1
        assert_output_contains "$platform mismatch" "$(<"$stderr_path")" "built for agent=$image_agent" || return 1
      done
    done
  done
}

test_entrypoint_declared_runner() {
  test_consumer_package_stdio_contract || return 1
  test_command_runner_package_contract || return 1
  test_declared_direct_runner_preserves_argv_stdio_both || return 1
  test_direct_executable_guard_both_entrypoints || return 1
  test_direct_metadata_missing_blocks_agent_both_entrypoints || return 1
  test_native_executable_missing_blocks_agent_both_entrypoints || return 1
  test_immutable_agent_variant_guard_both_entrypoints || return 1
  test_selected_native_agents_receive_argv_both_entrypoints || return 1
  test_interactive_claude_without_prompt_mount_both_entrypoints || return 1
  test_interactive_claude_mounted_prompt_both_entrypoints || return 1
  test_interactive_claude_invalid_prompt_blocks_agent_both_entrypoints
}

test_selected_native_agents_receive_argv_both_entrypoints() {
  require_command jq
  local platform agent stdio selected expected workspace stdout_path stderr_path binary
  for platform in linux darwin; do
    for agent in claude pi; do
      for stdio in 0 1; do
        workspace="$TEST_TMP/agent-$platform-$agent-$stdio/workspace"
        stdout_path="$TEST_TMP/agent-$platform-$agent-$stdio.out"
        stderr_path="$TEST_TMP/agent-$platform-$agent-$stdio.err"
        binary="$(agent_binary "$agent")"
        mkdir -p "$workspace/bin"
        write_bash_fixture "$workspace/bin/$binary" <<EOF
set -euo pipefail
jq -nc --args '\$ARGS.positional' -- "\$@"
EOF
        chmod +x "$workspace/bin/$binary"
        selected="$agent"
        if [[ "$stdio" == 0 ]]; then selected=""; fi
        if ! WRIX_TEST_STDIO="$stdio" WRIX_TEST_SELECTED_AGENT="$selected" \
          run_entrypoint "$platform" "$agent" "$stdout_path" "$stderr_path" "$workspace" 'two words' '' --help; then
          fail "$platform $agent entrypoint failed: $(<"$stderr_path")"
          return 1
        fi
        case "$agent:$stdio" in
          claude:1) expected='["--dangerously-skip-permissions","--print","--verbose","--input-format","stream-json","--output-format","stream-json","two words","","--help"]' ;;
          claude:0) expected='["--dangerously-skip-permissions","two words","","--help"]' ;;
          pi:1) expected='["--mode","rpc","two words","","--help"]' ;;
          pi:0) expected='["two words","","--help"]' ;;
        esac
        jq -e --argjson expected "$expected" '. == $expected' "$stdout_path" >/dev/null || return 1
      done
    done
  done
  printf 'PASS: selected Pi/Claude receive argv in interactive and stdio modes on both platforms\n' >&2
}

write_claude_argv_probe() {
  local workspace="$1"
  mkdir -p "$workspace/bin"
  write_bash_fixture "$workspace/bin/claude" <<'EOF'
set -euo pipefail
jq -n --args '$ARGS.positional' -- "$@"
EOF
  chmod +x "$workspace/bin/claude"
}

assert_interactive_claude_launch() {
  local platform="$1"
  local workspace="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  local expected_prompt="$5"
  local expected_args
  local -a log_files

  if ! WRIX_TEST_STDIO=0 run_entrypoint "$platform" claude "$stdout_path" "$stderr_path" "$workspace"; then
    fail "$platform interactive Claude failed: $(<"$stderr_path")"
    return 1
  fi
  expected_args=$(jq -nc --arg prompt "$expected_prompt" '
    ["--dangerously-skip-permissions"]
    + if $prompt == "" then [] else ["--append-system-prompt", $prompt] end
  ')
  if ! jq -e --argjson expected "$expected_args" '. == $expected' "$stdout_path" >/dev/null; then
    fail "$platform interactive Claude argv differs: $(<"$stdout_path")"
    return 1
  fi
  mapfile -t log_files < <(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f)
  if [[ "${#log_files[@]}" -ne 1 ]] || ! jq -e --arg session_dir "$workspace/.claude" \
    '.exit_code == 0 and .agent_session_dir == $session_dir' "${log_files[0]}" >/dev/null; then
    fail "$platform interactive Claude did not write one successful audit index"
    return 1
  fi
}

test_interactive_claude_without_prompt_mount_both_entrypoints() {
  require_command jq
  local platform context workspace stdout_path stderr_path expected_prompt
  for platform in linux darwin; do
    for context in absent readme; do
      workspace="$TEST_TMP/interactive-$platform-$context/workspace"
      stdout_path="$TEST_TMP/interactive-$platform-$context.out"
      stderr_path="$TEST_TMP/interactive-$platform-$context.err"
      write_claude_argv_probe "$workspace"
      expected_prompt=""
      if [[ "$context" == readme ]]; then
        mkdir -p "$workspace/docs"
        printf 'Project context with spaces\n' >"$workspace/docs/README.md"
        expected_prompt="

## Project Context (from docs/README.md)

Project context with spaces"
      fi
      assert_interactive_claude_launch "$platform" "$workspace" "$stdout_path" "$stderr_path" "$expected_prompt" || return 1
    done
  done
  printf 'PASS: interactive Claude starts without a prompt mount and retains README context on both platforms\n' >&2
}

test_interactive_claude_mounted_prompt_both_entrypoints() {
  require_command jq
  local platform context workspace stdout_path stderr_path case_dir prompt_path expected_prompt
  for platform in linux darwin; do
    for context in absent readme; do
      workspace="$TEST_TMP/mounted-$platform-$context/workspace"
      stdout_path="$TEST_TMP/mounted-$platform-$context.out"
      stderr_path="$TEST_TMP/mounted-$platform-$context.err"
      case_dir="$TEST_TMP/$platform-claude-$(basename "$stdout_path" .out)"
      case "$platform" in
        linux) prompt_path="$case_dir/etc/wrix-prompt" ;;
        darwin) prompt_path="$case_dir/etc/wrix-prompts/wrix-prompt" ;;
      esac
      mkdir -p "$(dirname "$prompt_path")"
      expected_prompt="Mounted prompt with spaces"
      printf '%s\n' "$expected_prompt" >"$prompt_path"
      write_claude_argv_probe "$workspace"
      if [[ "$context" == readme ]]; then
        mkdir -p "$workspace/docs"
        printf 'Project context\n' >"$workspace/docs/README.md"
        expected_prompt="$expected_prompt

## Project Context (from docs/README.md)

Project context"
      fi
      assert_interactive_claude_launch "$platform" "$workspace" "$stdout_path" "$stderr_path" "$expected_prompt" || return 1
    done
  done
  printf 'PASS: both interactive entrypoints preserve mounted prompts and README augmentation as one argv value\n' >&2
}

test_interactive_claude_invalid_prompt_blocks_agent_both_entrypoints() {
  require_command jq
  local platform invalid workspace stdout_path stderr_path case_dir prompt_path
  for platform in linux darwin; do
    for invalid in directory dangling; do
      workspace="$TEST_TMP/invalid-$platform-$invalid/workspace"
      stdout_path="$TEST_TMP/invalid-$platform-$invalid.out"
      stderr_path="$TEST_TMP/invalid-$platform-$invalid.err"
      case_dir="$TEST_TMP/$platform-claude-$(basename "$stdout_path" .out)"
      case "$platform" in
        linux) prompt_path="$case_dir/etc/wrix-prompt" ;;
        darwin) prompt_path="$case_dir/etc/wrix-prompts/wrix-prompt" ;;
      esac
      mkdir -p "$(dirname "$prompt_path")"
      if [[ "$invalid" == directory ]]; then
        mkdir "$prompt_path"
      else
        ln -s "$case_dir/missing-prompt" "$prompt_path"
      fi
      write_claude_argv_probe "$workspace"
      if WRIX_TEST_STDIO=0 run_entrypoint "$platform" claude "$stdout_path" "$stderr_path" "$workspace"; then
        fail "$platform interactive Claude accepted a $invalid prompt"
        return 1
      fi
      if [[ -s "$stdout_path" ]]; then
        fail "$platform interactive Claude ran despite an invalid prompt"
        return 1
      fi
      assert_output_contains "$platform invalid prompt" "$(<"$stderr_path")" "$prompt_path" || return 1
    done
  done
  printf 'PASS: both interactive entrypoints reject invalid existing prompt inputs before running Claude\n' >&2
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

test_deploy_key_public_derivation_both_entrypoints() {
  require_command jq
  require_command ssh-keygen
  local key="$TEST_TMP/deploy-key"
  local expected platform

  ssh-keygen -q -t ed25519 -N '' -f "$key"
  expected=$(ssh-keygen -y -f "$key")
  rm -f "$key.pub"
  export WRIX_DEPLOY_KEY="$key"
  for platform in linux darwin; do
    local workspace="$TEST_TMP/deploy-key-$platform/workspace"
    local stdout_path="$TEST_TMP/deploy-key-$platform.out"
    local stderr_path="$TEST_TMP/deploy-key-$platform.err"
    local output
    # shellcheck disable=SC2016 # The entrypoint's inner shell expands the mounted key path.
    if ! run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
      bash -c 'ssh-keygen -y -f "$WRIX_DEPLOY_KEY"'; then
      unset WRIX_DEPLOY_KEY
      fail "$platform deploy-key derivation failed: $(<"$stderr_path")"
      return 1
    fi
    output=$(<"$stdout_path")
    if [[ "$output" != "$expected" ]]; then
      unset WRIX_DEPLOY_KEY
      fail "$platform derived the wrong deploy public key"
      return 1
    fi
  done
  unset WRIX_DEPLOY_KEY
  printf 'PASS: both entrypoints can derive the unmounted deploy public key\n' >&2
}

test_runtime_mcp_registration_uses_claude_user_config_both_entrypoints() {
  require_command jq
  local platform agent canonical_manifest=""
  for platform in linux darwin; do
    for agent in direct claude pi; do
      local workspace="$TEST_TMP/runtime-mcp-$platform-$agent/workspace"
      local stdout_path="$TEST_TMP/runtime-mcp-$platform-$agent.out"
      local stderr_path="$TEST_TMP/runtime-mcp-$platform-$agent.err"
      local case_dir home_dir manifest
      case_dir="$TEST_TMP/$platform-$agent-$(basename "$stdout_path" .out)"
      home_dir="$case_dir/home"

      # shellcheck disable=SC2016 # The entrypoint's inner shell expands the manifest path.
      if ! WRIX_TEST_MCP_RUNTIME=1 \
        WRIX_TEST_MCP_SELECTION=tmux \
        WRIX_TEST_MCP_TMUX_AUDIT=/workspace/.debug-audit.log \
        WRIX_TEST_MCP_TMUX_AUDIT_FULL=/workspace/.debug-audit \
        run_entrypoint "$platform" "$agent" "$stdout_path" "$stderr_path" "$workspace" \
        bash -c 'cat "$WRIX_MCP_MANIFEST"'; then
        fail "$platform $agent runtime MCP entrypoint failed: $(<"$stderr_path")"
        return 1
      fi
      if ! jq -e '
        .schema == 1
        and (.servers | length) == 1
        and .servers[0].name == "tmux"
        and .servers[0].command == "tmux-mcp"
        and .servers[0].args == []
        and .servers[0].env.TMUX_DEBUG_AUDIT == "/workspace/.debug-audit.log"
        and .servers[0].env.TMUX_DEBUG_AUDIT_FULL == "/workspace/.debug-audit"
      ' "$stdout_path" >/dev/null; then
        fail "$platform $agent did not receive the selected MCP manifest"
        return 1
      fi
      manifest=$(jq -cS . "$stdout_path")
      if [[ -z "$canonical_manifest" ]]; then
        canonical_manifest="$manifest"
      elif [[ "$manifest" != "$canonical_manifest" ]]; then
        fail "$platform $agent MCP manifest differs across agent adapters"
        return 1
      fi

      if [[ "$agent" == "claude" ]]; then
        if ! jq -e '
          .mcpServers.tmux.command == "tmux-mcp"
          and .mcpServers.tmux.args == []
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
      fi
    done
  done

  local explicit_workspace="$TEST_TMP/explicit-mcp/workspace"
  local explicit_stdout="$TEST_TMP/explicit-mcp.out"
  local explicit_stderr="$TEST_TMP/explicit-mcp.err"
  local explicit_manifest
  # shellcheck disable=SC2016 # The entrypoint's inner shell expands the manifest path.
  if ! WRIX_TEST_MCP_RUNTIME=1 \
    WRIX_TEST_MCP_RUNTIME_SELECTION=false \
    WRIX_TEST_MCP_SELECTION=unselected \
    WRIX_TEST_MCP_TMUX_AUDIT=/workspace/.debug-audit.log \
    WRIX_TEST_MCP_TMUX_AUDIT_FULL=/workspace/.debug-audit \
    run_entrypoint linux direct "$explicit_stdout" "$explicit_stderr" "$explicit_workspace" \
    bash -c 'cat "$WRIX_MCP_MANIFEST"'; then
    fail "explicit MCP entrypoint failed: $(<"$explicit_stderr")"
    return 1
  fi
  explicit_manifest=$(jq -cS . "$explicit_stdout")
  if [[ "$explicit_manifest" != "$canonical_manifest" ]]; then
    fail "explicit and runtime MCP selection produced different manifests"
    return 1
  fi

  printf 'PASS: explicit/runtime selection gives every agent one manifest and adapts Claude\n' >&2
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

  if ! WRIX_TEST_MCP_RUNTIME=1 WRIX_TEST_REAL_NATIVE_AGENT=1 WRIX_TEST_STDIO=0 \
    WRIX_TEST_MCP_SELECTION=tmux \
    run_entrypoint linux claude "$stdout_path" "$stderr_path" "$workspace" \
    mcp get tmux; then
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
  local git_layout="${2:-directory}"
  local workspace="$TEST_TMP/hooks-$platform-$git_layout/workspace"
  local stdout_path="$TEST_TMP/hooks-$platform-$git_layout.out"
  local stderr_path="$TEST_TMP/hooks-$platform-$git_layout.err"
  local git_log="$TEST_TMP/hooks-$platform-$git_layout.git.log"
  local hooks_path="$TEST_TMP/hooks-$platform-$git_layout/prek-hooks"

  mkdir -p "$hooks_path"
  case "$git_layout" in
    directory)
      mkdir -p "$workspace"
      git -C "$workspace" init -q -b main
      ;;
    linked-worktree)
      local primary="$TEST_TMP/hooks-$platform-$git_layout/primary"
      mkdir -p "$primary"
      git -C "$primary" init -q -b main
      git -C "$primary" -c user.name=Test -c user.email=test@example.invalid \
        commit --allow-empty -qm initial
      git -C "$primary" -c core.hooksPath=/dev/null \
        worktree add -q -b linked "$workspace"
      ;;
    *)
      fail "unknown git layout: $git_layout"
      return 1
      ;;
  esac
  printf 'repos: []\n' >"$workspace/.pre-commit-config.yaml"
  : >"$git_log"

  WRIX_PREK_HOOKS="$hooks_path"
  WRIX_PREK_RUNNER=$(command -v wrix-prek)
  WRIX_FAKE_GIT_LOG="$git_log"
  export WRIX_PREK_HOOKS WRIX_PREK_RUNNER WRIX_FAKE_GIT_LOG
  if ! run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" true; then
    unset WRIX_PREK_HOOKS WRIX_PREK_RUNNER WRIX_FAKE_GIT_LOG
    fail "$platform entrypoint failed: $(<"$stderr_path")"
    return 1
  fi
  unset WRIX_PREK_HOOKS WRIX_PREK_RUNNER WRIX_FAKE_GIT_LOG

  local system runner
  system=$(nix eval --raw --impure --expr 'builtins.currentSystem')
  runner=$(git -C "$workspace" config --local --get "wrix.prek-container-$system.runner")
  if [[ ! -x "$runner" ]]; then
    fail "$platform entrypoint did not bind its packaged container runner"
    return 1
  fi
  if ! grep -qxF -- "-C $workspace config --local core.hooksPath $hooks_path" "$git_log"; then
    fail "$platform entrypoint did not configure core.hooksPath to WRIX_PREK_HOOKS; git log: $(<"$git_log")"
    return 1
  fi
}

test_missing_hook_runtime_blocks_agent_both() {
  local platform dependency workspace stdout_path stderr_path hooks_path runner
  require_command git
  require_command jq
  for platform in linux darwin; do
    for dependency in runner bundle; do
      workspace="$TEST_TMP/missing-hooks-$platform-$dependency/workspace"
      stdout_path="$TEST_TMP/missing-hooks-$platform-$dependency.out"
      stderr_path="$TEST_TMP/missing-hooks-$platform-$dependency.err"
      hooks_path="$TEST_TMP/missing-hooks-$platform-$dependency/bundle"
      mkdir -p "$workspace" "$hooks_path"
      git -C "$workspace" init -q
      printf 'repos: []\n' >"$workspace/.pre-commit-config.yaml"
      runner=$(command -v wrix-prek)
      if [[ "$dependency" == runner ]]; then
        runner="$TEST_TMP/missing-runner"
      else
        hooks_path="$TEST_TMP/missing-bundle"
      fi
      if WRIX_PREK_HOOKS="$hooks_path" WRIX_PREK_RUNNER="$runner" \
        run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
        bash -c 'printf AGENT_RAN'; then
        fail "$platform entrypoint accepted missing hook $dependency"
        return 1
      fi
      assert_output_contains "$platform missing hook $dependency" "$(<"$stderr_path")" "rebuild the Wrix worker image"
      if [[ "$(<"$stdout_path")" == *AGENT_RAN* ]]; then
        fail "$platform entrypoint ran the agent without its hook runtime"
        return 1
      fi
    done
  done
}

test_linux_core_hooks_path() {
  require_command jq
  run_core_hooks_path_case linux || return "$?"
  printf 'PASS: linux entrypoint configures core.hooksPath when pre-commit config is present\n' >&2
}

test_darwin_core_hooks_path() {
  require_command jq
  run_core_hooks_path_case darwin || return "$?"
  printf 'PASS: darwin entrypoint configures core.hooksPath when pre-commit config is present\n' >&2
}

test_linked_worktree_core_hooks_path_both() {
  require_command git
  require_command jq
  local platform
  for platform in linux darwin; do
    run_core_hooks_path_case "$platform" linked-worktree || return "$?"
  done
  printf 'PASS: both entrypoints configure core.hooksPath in linked worktrees\n' >&2
}

test_darwin_file_mount_modes_sync_only_writable_files() {
  require_command jq
  local workspace="$TEST_TMP/file-mount-darwin/workspace"
  local staging="$TEST_TMP/file-mount-darwin/staging"
  local read_only_source="$staging/read-only"
  local writable_source="$staging/writable"
  local sibling="$staging/sibling-secret"
  local read_only_dest="$workspace/read-only"
  local writable_dest="$workspace/writable"
  local stdout_path="$TEST_TMP/file-mount-darwin.out"
  local stderr_path="$TEST_TMP/file-mount-darwin.err"
  local entrypoint_status=0
  mkdir -p "$workspace" "$staging"
  printf 'read-only-content\n' >"$read_only_source"
  printf 'writable-content\n' >"$writable_source"
  printf 'private\n' >"$sibling"

  export WRIX_FILE_MOUNTS="$read_only_source:$read_only_dest:ro,$writable_source:$writable_dest:rw"
  # shellcheck disable=SC2016 # The fixture runner expands its positional arguments.
  run_entrypoint darwin direct "$stdout_path" "$stderr_path" "$workspace" \
    /bin/bash -c '
      set -euo pipefail
      [[ -L "$1" ]]
      [[ "$(readlink "$1")" == "$3" ]]
      [[ "$(<"$1")" == "read-only-content" ]]
      [[ ! -L "$2" ]]
      printf "updated-content\n" >"$2"
    ' probe "$read_only_dest" "$writable_dest" "$read_only_source" || entrypoint_status=$?
  unset WRIX_FILE_MOUNTS

  if [[ "$entrypoint_status" -ne 0 ]]; then
    fail "Darwin file mount entrypoint failed: $(<"$stderr_path")"
    return 1
  fi
  [[ "$(<"$read_only_source")" == "read-only-content" ]] || {
    fail "Darwin read-only file source changed"
    return 1
  }
  [[ "$(<"$writable_source")" == "updated-content" ]] || {
    fail "Darwin writable file source was not synchronized"
    return 1
  }
  [[ "$(<"$sibling")" == "private" ]] || {
    fail "Darwin file synchronization changed an unselected sibling"
    return 1
  }
  printf 'PASS: Darwin file mounts preserve read-only mode and sync only writable files\n' >&2
}

test_darwin_bd_remote_remap() {
  require_command jq
  local workspace="$TEST_TMP/darwin-bd-remap/workspace"
  local stdout_path="$TEST_TMP/darwin-bd-remap.out"
  local stderr_path="$TEST_TMP/darwin-bd-remap.err"
  local log_file="$TEST_TMP/darwin-bd-remap.log"
  local state_file="$TEST_TMP/darwin-bd-remap.state"
  local original_remote="file:///host-checkout/.git/beads-worktrees/beads/.beads/dolt-remote"
  local expected_remote="file://$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  mkdir -p "$workspace/.beads/dolt" "$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  printf '%s\n' 'sync-branch: "beads"' >"$workspace/.beads/config.yaml"
  printf '%s\n' '{"backend":"dolt"}' >"$workspace/.beads/metadata.json"
  printf '%s\n' "$original_remote" >"$state_file"
  : >"$log_file"

  export BEADS_DOLT_SERVER_HOST=127.0.0.1
  export BEADS_DOLT_SERVER_PORT=3307
  export WRIX_EXPECTED_BD_REMOTE="$expected_remote"
  export WRIX_FAKE_BD_LOG="$log_file"
  export WRIX_FAKE_BD_STATE="$state_file"
  export WRIX_ORIGINAL_BD_REMOTE="$original_remote"
  local operation
  for operation in pull push; do
    if ! run_entrypoint darwin direct "$stdout_path" "$stderr_path" "$workspace" bd dolt "$operation"; then
      unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT WRIX_EXPECTED_BD_REMOTE
      unset WRIX_FAKE_BD_LOG WRIX_FAKE_BD_STATE WRIX_ORIGINAL_BD_REMOTE
      fail "Darwin bd remote remap failed for $operation: $(<"$stderr_path")"
      return 1
    fi
    assert_output_contains "Darwin remapped $operation" "$(<"$log_file")" "$operation-origin=$expected_remote" || return 1
  done
  unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT WRIX_EXPECTED_BD_REMOTE
  unset WRIX_FAKE_BD_LOG WRIX_FAKE_BD_STATE WRIX_ORIGINAL_BD_REMOTE

  if [[ "$(<"$state_file")" != "$original_remote" ]]; then
    fail "Darwin bd wrapper did not restore the original remote"
    return 1
  fi
  printf 'PASS: Darwin entrypoint remaps and restores the Dolt origin around pull and push\n' >&2
}

test_stale_beads_endpoint_blocks_agent_both() {
  local platform workspace stdout_path stderr_path
  for platform in linux darwin; do
    workspace="$TEST_TMP/stale-beads-$platform"
    stdout_path="$workspace.out"
    stderr_path="$workspace.err"
    mkdir -p "$workspace/.beads" "$workspace/bin"
    printf '%s\n' '{"backend":"dolt","dolt_mode":"server"}' >"$workspace/.beads/metadata.json"
    printf 'sync.mode: dolt-native\n' >"$workspace/.beads/config.yaml"
    printf 'set -euo pipefail\ntouch %q\n' "$workspace/agent-ran" \
      | write_bash_fixture "$workspace/bin/consumer-agent"
    chmod +x "$workspace/bin/consumer-agent"
    export BEADS_DOLT_SERVER_HOST=192.0.2.10 BEADS_DOLT_SERVER_PORT=24470
    export WRIX_FAKE_BD_UNREACHABLE=1 WRIX_FAKE_BD_LOG="$workspace.bd-log" WRIX_FAKE_BD_STATE="$workspace.bd-state"
    if run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace"; then
      fail "$platform accepted an unreachable Dolt endpoint"
      return 1
    fi
    [[ ! -e "$workspace/agent-ran" ]] || { fail "$platform launched the agent before SQL readiness"; return 1; }
    assert_output_contains "$platform diagnostic" "$(<"$stderr_path")" 'from this sandbox' || return 1
    assert_output_contains "$platform cause" "$(<"$stderr_path")" 'connection refused' || return 1
  done
  unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT WRIX_FAKE_BD_UNREACHABLE WRIX_FAKE_BD_LOG WRIX_FAKE_BD_STATE
}

test_entrypoints_require_network_bootstrap() {
  local platform failure workspace stdout_path stderr_path diagnostic ready caps field
  for platform in linux darwin; do
    for failure in CapInh CapPrm CapEff CapBnd CapAmb malformed marker; do
      workspace="$TEST_TMP/bootstrap-$platform-$failure/workspace"
      stdout_path="$TEST_TMP/bootstrap-$platform-$failure.out"
      stderr_path="$TEST_TMP/bootstrap-$platform-$failure.err"
      ready=1
      field="$failure"
      caps=0000000000001000
      diagnostic='NET_ADMIN survived the network bootstrap'
      if [[ "$failure" == marker ]]; then
        ready=0
        caps=0000000000000000
        diagnostic='network bootstrap did not complete'
      elif [[ "$failure" == malformed ]]; then
        caps=invalid
        diagnostic='invalid Linux capability state'
        field=CapEff
      fi
      if WRIX_TEST_NETWORK_READY="$ready" WRIX_TEST_CAP_STATUS_HEX="$caps" WRIX_TEST_CAP_FIELD="$field" \
        run_entrypoint "$platform" direct "$stdout_path" "$stderr_path" "$workspace" \
        touch "$workspace/agent-ran"; then
        fail "$platform accepted an unsafe bootstrap: $failure"
        return 1
      fi
      [[ ! -e "$workspace/agent-ran" ]] || { fail "$platform ran the agent"; return 1; }
      [[ -z "$(find "$workspace/.wrix/log" -type f -print)" ]] || {
        fail "$platform ran exit logging after a bootstrap rejection"
        return 1
      }
      assert_output_contains "$platform bootstrap rejection" "$(<"$stderr_path")" "$diagnostic" || return 1
    done
  done
  printf 'PASS: both entrypoints reject unsafe startup before setup or exit logging\n' >&2
}

test_same_second_audit_indexes_both_entrypoints() {
  require_command jq
  # shellcheck source=tests/security/audit-clock.sh
  source "$REPO_ROOT/tests/security/audit-clock.sh"
  local clock_date="${WRIX_TEST_AUDIT_CLOCK_DATE:?}" platform member workspace barrier case_dir tool_dir home_dir etc_wrix entrypoint
  local first_pid second_pid ready_status first_status second_status
  local -a pids log_files

  for platform in linux darwin; do
    workspace="$TEST_TMP/clock-$platform/workspace"
    barrier="$workspace/.wrix/audit-start"
    mkdir -p "$barrier"
    AUDIT_CLOCK_BARRIERS+=("$barrier")
    pids=()
    for member in first second; do
      case_dir="$TEST_TMP/clock-$platform-$member"
      tool_dir="$case_dir/tools"
      home_dir="$case_dir/home"
      etc_wrix="$case_dir/etc/wrix"
      entrypoint="$case_dir/entrypoint.sh"
      mkdir -p "$home_dir"
      write_fake_runtime_tools "$tool_dir"
      prepare_wrix_etc "$etc_wrix" direct
      rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint" "$home_dir"
    done
    for member in first second; do
      case_dir="$TEST_TMP/clock-$platform-$member"
      (
        if [[ "$member" = second ]]; then sleep 1.2; fi
        env HOME="$case_dir/home" HOST_UID="$(id -u)" \
          PATH="$case_dir/tools:$(dirname "$clock_date"):$PATH" \
          WRIX_AGENT=direct WRIX_FIREWALL_BACKEND=iptables WRIX_NETWORK=open \
          WRIX_SESSION_ID="$platform-$member" \
          WRIX_TEST_AUDIT_START_BARRIER="$barrier" WRIX_TEST_AUDIT_START_MEMBER="$member" \
          bash "$case_dir/entrypoint.sh" true >"$case_dir.out" 2>"$case_dir.err"
      ) &
      pids+=("$!")
      AUDIT_CLOCK_PIDS+=("$!")
    done
    first_pid="${pids[0]}"
    second_pid="${pids[1]}"
    ready_status=0
    audit_wait_for_start_clocks "$barrier" "$first_pid" "$second_pid" || ready_status=$?
    if [[ "$ready_status" -eq 0 ]]; then
      audit_release_start_clocks "$barrier"
    else
      : >"$barrier/release"
    fi
    first_status=0
    second_status=0
    wait "$first_pid" || first_status=$?
    wait "$second_pid" || second_status=$?
    AUDIT_CLOCK_PIDS=()
    if [[ "$ready_status" -ne 0 || "$first_status" -ne 0 || "$second_status" -ne 0 ]]; then
      fail "$platform coordinated entrypoints failed ($ready_status, $first_status, $second_status)"
      return 1
    fi
    mapfile -t log_files < <(find "$workspace/.wrix/log" -maxdepth 1 -name '*.json' -type f)
    if [[ "${#log_files[@]}" -ne 2 ]] || ! jq -es --arg platform "$platform" '
      length == 2
      and ([.[].timestamp_start] | unique | length == 1)
      and ([.[].wrix_session_id] | sort == [$platform + "-first", $platform + "-second"])
      and all(.[]; .exit_code == 0)
    ' "${log_files[@]}" >/dev/null; then
      fail "$platform did not retain two distinct same-second audit indexes"
      return 1
    fi
    printf 'PASS: %s delayed entrypoint starts retain two real same-second indexes\n' "$platform"
  done
}

ALL_TESTS=(
  test_runtime_fixtures_do_not_need_env_or_path
  test_workspace_bin_path_prepend_both
  test_selected_native_agents_receive_argv_both_entrypoints
  test_direct_executable_guard_both_entrypoints
  test_direct_metadata_missing_blocks_agent_both_entrypoints
  test_native_executable_missing_blocks_agent_both_entrypoints
  test_immutable_agent_variant_guard_both_entrypoints
  test_interactive_claude_without_prompt_mount_both_entrypoints
  test_interactive_claude_mounted_prompt_both_entrypoints
  test_interactive_claude_invalid_prompt_blocks_agent_both_entrypoints
  test_agent_config_homes_both_entrypoints
  test_deploy_key_public_derivation_both_entrypoints
  test_runtime_mcp_registration_uses_claude_user_config_both_entrypoints
  test_linux_core_hooks_path
  test_darwin_core_hooks_path
  test_linked_worktree_core_hooks_path_both
  test_darwin_bd_remote_remap
  test_stale_beads_endpoint_blocks_agent_both
  test_darwin_file_mount_modes_sync_only_writable_files
  test_entrypoints_require_network_bootstrap
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
