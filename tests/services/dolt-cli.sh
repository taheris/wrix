#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-dolt-cli.XXXXXX)"
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
    printf 'SKIP: %s is required\n' "$command_name" >&2
    exit 77
  fi
}

entrypoint_agent() {
  if [[ -f /etc/wrix/image-agent ]]; then
    tr -d '\n' </etc/wrix/image-agent
  else
    printf 'direct\n'
  fi
}

assert_path_exists() {
  local path="$1"
  if [[ ! -e "$path" ]]; then
    fail "expected path to exist: $path"
  fi
}

assert_path_absent() {
  local path="$1"
  if [[ -e "$path" ]]; then
    fail "expected path to be absent: $path"
  fi
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

write_fake_container_runtime_tools() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"

  cat >"$bin_dir/yq" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *'sync-branch'* ]]; then
  printf 'beads\n'
else
  printf 'wx\n'
fi
EOF
  chmod +x "$bin_dir/yq"

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
if [[ "$script" == *'-A OUTPUT -j ACCEPT'* ]]; then
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
printf '198.51.100.9 STREAM %s\n' "${2:-localhost}"
EOF
  chmod +x "$bin_dir/getent"
}

write_fake_container_tools() {
  local bin_dir="$1"
  local log_file="$2"
  write_fake_container_runtime_tools "$bin_dir"

  cat >"$bin_dir/bd" <<EOF
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "\$*" >>"$log_file"
exit 9
EOF
  chmod +x "$bin_dir/bd"
}

rewrite_entrypoint_workspace() {
  local source_path="$1"
  local workspace="$2"
  local dest_path="$3"
  python3 - "$source_path" "$workspace" "$dest_path" <<'PY'
from pathlib import Path
import shlex
import sys
source = Path(sys.argv[1])
workspace = Path(sys.argv[2])
dest = Path(sys.argv[3])
setup = workspace / 'git-ssh-setup.sh'
setup.write_text('#!/usr/bin/env bash\nset -euo pipefail\n', encoding='utf-8')
setup.chmod(0o755)
mcp_setup = workspace / 'mcp-manifest.sh'
mcp_source = source.parent.parent / 'mcp-manifest.sh'
mcp_setup.write_text(mcp_source.read_text(encoding='utf-8'), encoding='utf-8')
cap_status = workspace / 'proc-self-status'
cap_status.write_text(
    'CapInh:\t0000000000000000\n'
    'CapPrm:\t0000000000000000\n'
    'CapEff:\t0000000000000000\n'
    'CapBnd:\t0000000000000000\n'
    'CapAmb:\t0000000000000000\n',
    encoding='utf-8',
)
network_ready = workspace / 'wrix-network-ready'
network_ready.touch()
ready_helper = workspace / 'network-ready.sh'
ready_helper.write_text(
    (source.parent.parent / 'network-ready.sh').read_text(encoding='utf-8')
    .replace('/proc/self/status', str(cap_status))
    .replace('/run/wrix-network-ready', str(network_ready)),
    encoding='utf-8',
)
text = source.read_text(encoding='utf-8').replace('/workspace', str(workspace))
text = text.replace('. /network-ready.sh', f'. {shlex.quote(str(ready_helper))}')
beads_helper = source.parent.parent.parent / 'beads/sandbox.sh'
text = text.replace('. /beads-sandbox.sh', f'. {shlex.quote(str(beads_helper))}')
text = text.replace('. /git-ssh-setup.sh', f'. {shlex.quote(str(setup))}')
text = text.replace('. /mcp-manifest.sh', f'. {shlex.quote(str(mcp_setup))}')
dest.write_text(text, encoding='utf-8')
PY
  chmod +x "$dest_path"
}

write_beads_files() {
  local workspace="$1"
  local backend="$2"
  mkdir -p "$workspace/.beads"
  cat >"$workspace/.beads/config.yaml" <<'EOF'
issue-prefix: wx
sync:
  mode: dolt-native
EOF
  cat >"$workspace/.beads/metadata.json" <<EOF
{"backend":"$backend","database":"$backend"}
EOF
  printf '{"id":"wx-1"}\n' >"$workspace/.beads/issues.jsonl"
}

run_entrypoint_command() {
  local entrypoint="$1"
  local workspace="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  shift 4
  local home_dir="$TEST_TMP/home"
  local xdg_config_home="$home_dir/.config"
  local xdg_cache_home="$home_dir/.cache"
  local xdg_state_home="$home_dir/.local/state"
  local agent
  local -a endpoint_env
  agent="$(entrypoint_agent)"
  endpoint_env=(
    -u BEADS_DOLT_SERVER_HOST
    -u BEADS_DOLT_SERVER_PORT
    -u BEADS_DOLT_SERVER_SOCKET
    -u BEADS_DOLT_AUTO_START
  )
  if [[ -n "${BEADS_TEST_DOLT_SOCKET:-}" ]]; then
    endpoint_env+=("BEADS_DOLT_SERVER_SOCKET=$BEADS_TEST_DOLT_SOCKET")
  fi
  if [[ -n "${BEADS_TEST_DOLT_HOST:-}" ]]; then
    endpoint_env+=("BEADS_DOLT_SERVER_HOST=$BEADS_TEST_DOLT_HOST")
  fi
  if [[ -n "${BEADS_TEST_DOLT_PORT:-}" ]]; then
    endpoint_env+=("BEADS_DOLT_SERVER_PORT=$BEADS_TEST_DOLT_PORT")
  fi
  mkdir -p "$home_dir" "$xdg_config_home" "$xdg_cache_home" "$xdg_state_home"
  env \
    "${endpoint_env[@]}" \
    HOME="$home_dir" \
    XDG_CONFIG_HOME="$xdg_config_home" \
    XDG_CACHE_HOME="$xdg_cache_home" \
    XDG_STATE_HOME="$xdg_state_home" \
    HOST_UID="$(id -u)" \
    WRIX_AGENT="$agent" \
    WRIX_FIREWALL_BACKEND=iptables \
    WRIX_NETWORK=open \
    PATH="$workspace/bin:$PATH" \
    bash "$entrypoint" "$@" >"$stdout_path" 2>"$stderr_path"
}

run_entrypoint() {
  local entrypoint="$1"
  local workspace="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  run_entrypoint_command "$entrypoint" "$workspace" "$stdout_path" "$stderr_path" echo ok
}

assert_dolt_endpoint_failure() {
  local platform="$1"
  local source_path="$2"
  local workspace="$TEST_TMP/$platform-missing-endpoint-workspace"
  local entrypoint="$TEST_TMP/$platform-entrypoint.sh"
  local bd_log="$TEST_TMP/$platform-bd.log"
  local stdout_path="$TEST_TMP/$platform-missing-endpoint.out"
  local stderr_path="$TEST_TMP/$platform-missing-endpoint.err"

  write_beads_files "$workspace" dolt
  write_fake_container_tools "$workspace/bin" "$bd_log"
  rewrite_entrypoint_workspace "$source_path" "$workspace" "$entrypoint"

  if run_entrypoint "$entrypoint" "$workspace" "$stdout_path" "$stderr_path"; then
    fail "$platform entrypoint succeeded without a Dolt endpoint"
  fi
  assert_contains "$platform missing endpoint" "$(<"$stderr_path")" "configured Dolt socket is unavailable"
  assert_path_absent "$bd_log"
}

assert_darwin_does_not_import_jsonl() {
  local workspace="$TEST_TMP/darwin-jsonl-workspace"
  local entrypoint="$TEST_TMP/darwin-jsonl-entrypoint.sh"
  local bd_log="$TEST_TMP/darwin-jsonl-bd.log"
  local stdout_path="$TEST_TMP/darwin-jsonl.out"
  local stderr_path="$TEST_TMP/darwin-jsonl.err"

  write_beads_files "$workspace" sqlite
  write_fake_container_tools "$workspace/bin" "$bd_log"
  rewrite_entrypoint_workspace "$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh" "$workspace" "$entrypoint"

  if ! run_entrypoint "$entrypoint" "$workspace" "$stdout_path" "$stderr_path"; then
    fail "darwin entrypoint failed for non-Dolt beads config: $(<"$stderr_path")"
  fi
  assert_contains "darwin command override" "$(<"$stdout_path")" "ok"
  assert_path_absent "$bd_log"
}

test_entrypoints_preserve_dirty_agents_documentation() {
  require_command git
  require_command jq
  require_command python3

  local platform workspace entrypoint bd_log stdout_path stderr_path agents_contents
  for platform in linux darwin; do
    workspace="$TEST_TMP/$platform-agents-workspace"
    entrypoint="$TEST_TMP/$platform-agents-entrypoint.sh"
    bd_log="$TEST_TMP/$platform-agents-bd.log"
    stdout_path="$TEST_TMP/$platform-agents.out"
    stderr_path="$TEST_TMP/$platform-agents.err"

    write_beads_files "$workspace" sqlite
    printf 'dolt/\n' >"$workspace/.beads/.gitignore"
    printf 'committed documentation\n' >"$workspace/AGENTS.md"
    git -C "$workspace" init -q -b main
    git -C "$workspace" add -- .beads/.gitignore AGENTS.md
    printf 'operator-owned edit\n' >"$workspace/AGENTS.md"

    write_fake_container_tools "$workspace/bin" "$bd_log"
    rewrite_entrypoint_workspace "$REPO_ROOT/lib/sandbox/$platform/entrypoint.sh" "$workspace" "$entrypoint"
    if ! run_entrypoint "$entrypoint" "$workspace" "$stdout_path" "$stderr_path"; then
      fail "$platform entrypoint failed: $(<"$stderr_path")"
      return 1
    fi

    agents_contents="$(<"$workspace/AGENTS.md")"
    if [[ "$agents_contents" != "operator-owned edit" ]]; then
      fail "$platform entrypoint replaced dirty AGENTS.md content: $agents_contents"
      return 1
    fi
  done
}

test_entrypoints_reject_embedded_dolt_and_jsonl_fallback() {
  require_command python3
  require_command jq

  assert_dolt_endpoint_failure linux "$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"
  assert_dolt_endpoint_failure darwin "$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh"
  assert_darwin_does_not_import_jsonl
}

ALL_TESTS=(
  test_entrypoints_preserve_dirty_agents_documentation
  test_entrypoints_reject_embedded_dolt_and_jsonl_fallback
)

run_all() {
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    "$fn"
    printf 'PASS: %s\n' "$fn"
  done
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
