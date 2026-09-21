#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-entrypoint-contract.XXXXXX)"
unset WRIX_DIR_MOUNTS WRIX_FILE_MOUNTS

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

  cat >"$bin_dir/getent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" != "ahostsv4" ]]; then
  exit 2
fi
printf '93.184.216.34 STREAM %s\n' "${2:-example.com}"
EOF
  chmod +x "$bin_dir/getent"

  cat >"$bin_dir/bd" <<'EOF'
#!/usr/bin/env bash
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
  local source_path setup_path mcp_helper capability_status ready_file capability_hex ready_helper field
  source_path="$(entrypoint_source "$platform")"
  setup_path="$workspace/git-ssh-setup.sh"
  mcp_helper="$workspace/mcp-manifest.sh"
  capability_status="$workspace/proc-self-status"
  ready_file="$workspace/network-ready"
  capability_hex="${WRIX_TEST_CAP_STATUS_HEX:-0000000000000000}"
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' >"$setup_path"
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
  printf '%s\n' "$agent" >"$etc_wrix/image-agent"
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
  rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint" "$home_dir"

  env \
    HOME="$home_dir" \
    HOST_UID="$(id -u)" \
    PATH="$tool_dir:$PATH" \
    WRIX_AGENT="$agent" \
    WRIX_FIREWALL_BACKEND=iptables \
    WRIX_MCP="${WRIX_TEST_MCP_SELECTION:-}" \
    WRIX_MCP_TMUX_AUDIT="${WRIX_TEST_MCP_TMUX_AUDIT:-}" \
    WRIX_MCP_TMUX_AUDIT_FULL="${WRIX_TEST_MCP_TMUX_AUDIT_FULL:-}" \
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
  local git_layout="${2:-directory}"
  local workspace="$TEST_TMP/hooks-$platform-$git_layout/workspace"
  local stdout_path="$TEST_TMP/hooks-$platform-$git_layout.out"
  local stderr_path="$TEST_TMP/hooks-$platform-$git_layout.err"
  local git_log="$TEST_TMP/hooks-$platform-$git_layout.git.log"
  local hooks_path="$TEST_TMP/hooks-$platform-$git_layout/prek-hooks"

  mkdir -p "$hooks_path"
  case "$git_layout" in
    directory)
      mkdir -p "$workspace/.git"
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

test_linked_worktree_core_hooks_path_both() {
  require_command git
  require_command jq
  local platform
  for platform in linux darwin; do
    run_core_hooks_path_case "$platform" linked-worktree
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
  # shellcheck disable=SC2016 # The command override expands its positional arguments.
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
    printf '#!/usr/bin/env bash\nset -euo pipefail\ntouch "%s/agent-ran"\n' "$workspace" >"$workspace/bin/loom-direct-runner"
    chmod +x "$workspace/bin/loom-direct-runner"
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

ALL_TESTS=(
  test_workspace_bin_path_prepend_both
  test_agent_dispatch_both_entrypoints
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
