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
  local source_path setup_path
  source_path="$(entrypoint_source "$platform")"
  setup_path="$workspace/git-ssh-setup.sh"
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' >"$setup_path"
  chmod +x "$setup_path"
  sed \
    -e "s|/workspace|$workspace|g" \
    -e "s|/etc/wrix/|$etc_wrix/|g" \
    -e "s|\. /git-ssh-setup\.sh|. $setup_path|g" \
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
  rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint"

  env \
    HOME="$home_dir" \
    HOST_UID="$(id -u)" \
    PATH="$tool_dir:$PATH" \
    WRIX_AGENT="$agent" \
    WRIX_FIREWALL_BACKEND=iptables \
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

ALL_TESTS=(
  test_workspace_bin_path_prepend_both
  test_agent_dispatch_both_entrypoints
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
