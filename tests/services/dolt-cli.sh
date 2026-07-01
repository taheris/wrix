#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-dolt-cli.XXXXXX)"
SOCKET_PIDS=()
DOLT_PIDS=()
cleanup() {
  local pid
  for pid in "${SOCKET_PIDS[@]}" "${DOLT_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true # best-effort: helper process may already be gone.
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

write_asserting_bd() {
  local bin_dir="$1"
  local log_file="$2"
  cat >"$bin_dir/bd" <<EOF
#!/usr/bin/env bash
set -euo pipefail
origin_file="$log_file.origin"
current_origin() {
  if [[ -f "\$origin_file" ]]; then
    cat "\$origin_file"
  else
    printf '%s' "\${FAKE_BD_ORIGIN_REMOTE:-}"
  fi
}
printf '%s|auto=%s|socket=%s|host=%s|port=%s|origin=%s\n' \
  "\$*" \
  "\${BEADS_DOLT_AUTO_START:-}" \
  "\${BEADS_DOLT_SERVER_SOCKET:-}" \
  "\${BEADS_DOLT_SERVER_HOST:-}" \
  "\${BEADS_DOLT_SERVER_PORT:-}" \
  "\$(current_origin)" >>"$log_file"
if [[ "\${1:-}" == "dolt" && "\${2:-}" == "remote" && "\${3:-}" == "list" ]]; then
  origin="\$(current_origin)"
  if [[ -n "\$origin" ]]; then
    printf 'origin %s\n' "\$origin"
  fi
  exit 0
fi
if [[ "\${1:-}" == "sql" ]]; then
  query=""
  argument=""
  for argument in "\$@"; do
    query="\$argument"
  done
  if [[ "\$query" == "CALL DOLT_REMOTE('remove', 'origin')" ]]; then
    rm -f "\$origin_file"
    exit 0
  fi
  if [[ "\$query" == "CALL DOLT_REMOTE('add', 'origin', '"*"')" ]]; then
    prefix="CALL DOLT_REMOTE('add', 'origin', '"
    suffix="')"
    remote_url="\${query#"\$prefix"}"
    remote_url="\${remote_url%"\$suffix"}"
    printf '%s\n' "\$remote_url" >"\$origin_file"
    exit 0
  fi
  printf 'unexpected bd sql: %s\n' "\$query" >&2
  exit 64
fi
case "\$*" in
  "dolt pull"|"dolt push") ;;
  *)
    printf 'unexpected bd invocation: %s\n' "\$*" >&2
    exit 64
    ;;
esac
if [[ -n "\${BD_EXPECT_ORIGIN:-}" && "\$(current_origin)" != "\$BD_EXPECT_ORIGIN" ]]; then
  printf 'origin %s, expected %s\n' "\$(current_origin)" "\$BD_EXPECT_ORIGIN" >&2
  exit 69
fi
if [[ "\${BEADS_DOLT_AUTO_START:-}" != "0" ]]; then
  printf 'BEADS_DOLT_AUTO_START was not disabled\n' >&2
  exit 65
fi
if [[ -n "\${BEADS_DOLT_SERVER_SOCKET:-}" ]]; then
  if [[ ! -S "\$BEADS_DOLT_SERVER_SOCKET" ]]; then
    printf 'Dolt socket is unavailable: %s\n' "\$BEADS_DOLT_SERVER_SOCKET" >&2
    exit 66
  fi
elif [[ -z "\${BEADS_DOLT_SERVER_HOST:-}" || -z "\${BEADS_DOLT_SERVER_PORT:-}" ]]; then
  printf 'no Dolt service endpoint was exported\n' >&2
  exit 67
fi
if [[ -e .beads/issues.jsonl ]]; then
  printf 'JSONL backup was staged into the sandbox\n' >&2
  exit 68
fi
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
text = source.read_text(encoding='utf-8').replace('/workspace', str(workspace))
text = text.replace('. /git-ssh-setup.sh', f'. {shlex.quote(str(setup))}')
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

run_stage_beads() {
  local workspace="$1"
  local staging_root="$2"
  local snippet
  snippet="$(nix eval --impure --raw --expr "let snippets = import $REPO_ROOT/lib/util/shell.nix {}; in snippets.stageBeads")"
  PROJECT_DIR="$workspace" STAGING_ROOT="$staging_root" bash -euo pipefail -c "$snippet
    [[ -n \"\$BEADS_STAGING\" ]]
    [[ -f \"\$BEADS_STAGING/config.yaml\" ]]
    [[ -f \"\$BEADS_STAGING/metadata.json\" ]]
    [[ ! -e \"\$BEADS_STAGING/issues.jsonl\" ]]
  "
}

start_unix_socket() {
  local socket_path="$1"
  rm -f "$socket_path"
  python3 - "$socket_path" <<'PY' &
import socket
import sys
import time

path = sys.argv[1]
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
time.sleep(60)
PY
  local pid="$!"
  SOCKET_PIDS+=("$pid")
  local attempt
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -S "$socket_path" ]]; then
      return 0
    fi
    sleep 0.05
  done
  fail "socket helper did not create $socket_path"
}

real_bd_bin() {
  if [[ -n "${WRIX_REAL_BD_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_REAL_BD_BIN"
  else
    command -v bd
  fi
}

allocate_tcp_port() {
  python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.bind(('127.0.0.1', 0))
    print(sock.getsockname()[1])
PY
}

start_dolt_server() {
  local data_dir="$1"
  local socket_path="$2"
  local port
  local server_stdout
  local server_stderr
  local pid
  local attempt
  port="$(allocate_tcp_port)"
  server_stdout="$socket_path.out"
  server_stderr="$socket_path.err"
  rm -f "$socket_path"
  dolt sql-server \
    --socket "$socket_path" \
    --host 127.0.0.1 \
    --port "$port" \
    --data-dir "$data_dir" \
    >"$server_stdout" 2>"$server_stderr" &
  pid="$!"
  DOLT_PIDS+=("$pid")
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -S "$socket_path" ]]; then
      return 0
    fi
    sleep 0.05
  done
  cat "$server_stdout" >&2
  cat "$server_stderr" >&2
  fail "dolt sql-server did not create $socket_path"
}

write_server_backed_beads() {
  local workspace="$1"
  local socket_path="$2"
  local real_bd="$3"
  local data_dir="$workspace/.beads/dolt"
  local init_stdout="$workspace/bd-init.out"
  local init_stderr="$workspace/bd-init.err"
  mkdir -p "$data_dir"
  git -C "$workspace" init -q
  (cd "$data_dir" && dolt init --name 'Wrix Test' --email 'wrix@example.invalid' >/dev/null)
  start_dolt_server "$data_dir" "$socket_path"
  if ! (cd "$workspace" && \
    BEADS_DOLT_SERVER_SOCKET="$socket_path" \
    BEADS_DOLT_AUTO_START=0 \
    "$real_bd" init \
      -p wx \
      --skip-hooks \
      --skip-agents \
      --non-interactive \
      --server \
      --server-socket "$socket_path" \
      --server-host 127.0.0.1 \
      --database wx \
      >"$init_stdout" 2>"$init_stderr"); then
    cat "$init_stdout" >&2
    cat "$init_stderr" >&2
    fail "bd init failed for server-backed beads fixture"
  fi
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

assert_launcher_exports_service_endpoint() {
  bash "$REPO_ROOT/tests/services/sandbox-nix-config.sh" test_loom_bead_spawn_uses_repo_service
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
  assert_contains "$platform missing endpoint" "$(<"$stderr_path")" "dolt backend configured but no connection available"
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

test_fake_bd_contract() {
  require_command python3
  local workspace="$TEST_TMP/fake-bd-workspace"
  local log_file="$TEST_TMP/fake-bd.log"
  local socket_path="$TEST_TMP/fake-bd.sock"
  mkdir -p "$workspace/bin" "$workspace/.beads"
  write_asserting_bd "$workspace/bin" "$log_file"
  start_unix_socket "$socket_path"

  (
    cd "$workspace"
    BEADS_DOLT_AUTO_START=0 \
      BEADS_DOLT_SERVER_SOCKET="$socket_path" \
      "$workspace/bin/bd" dolt pull
  )

  assert_contains "fake bd log" "$(<"$log_file")" "dolt pull|auto=0|socket=$socket_path"
}

test_bd_dolt_sync_uses_container_remote() {
  require_command python3
  local workspace="$TEST_TMP/remote-wrapper-workspace"
  local entrypoint="$TEST_TMP/remote-wrapper-entrypoint.sh"
  local bd_log="$TEST_TMP/remote-wrapper-bd.log"
  local stdout_path="$TEST_TMP/remote-wrapper.out"
  local stderr_path="$TEST_TMP/remote-wrapper.err"
  local socket_path="$TEST_TMP/remote-wrapper-dolt.sock"
  local real_bd_dir="$TEST_TMP/remote-wrapper-real-bd-bin"
  local old_path="$PATH"
  local container_remote
  local host_remote

  write_beads_files "$workspace" dolt
  rm -f "$workspace/.beads/issues.jsonl"
  mkdir -p "$workspace/bin" "$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  write_fake_container_tools "$workspace/bin" "$bd_log"
  write_asserting_bd "$workspace/bin" "$bd_log"
  mkdir -p "$real_bd_dir"
  mv "$workspace/bin/bd" "$real_bd_dir/bd"
  export PATH="$real_bd_dir:$PATH"
  rewrite_entrypoint_workspace "$REPO_ROOT/lib/sandbox/linux/entrypoint.sh" "$workspace" "$entrypoint"
  start_unix_socket "$socket_path"
  container_remote="file://$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  host_remote="file:///home/shaun/src/github.com/taheris/wrix/.git/beads-worktrees/beads/.beads/dolt-remote"

  export FAKE_BD_ORIGIN_REMOTE="$host_remote"
  export BD_EXPECT_ORIGIN="$container_remote"
  if ! BEADS_TEST_DOLT_SOCKET="$socket_path" \
    run_entrypoint_command "$entrypoint" "$workspace" "$stdout_path" "$stderr_path" \
      bash -c 'bd dolt pull && bd dolt push'; then
    fail "entrypoint failed to remap bd remote: $(<"$stderr_path")"
  fi
  unset FAKE_BD_ORIGIN_REMOTE BD_EXPECT_ORIGIN
  export PATH="$old_path"

  assert_contains "bd remote add" "$(<"$bd_log")" "CALL DOLT_REMOTE('add', 'origin', '$container_remote')"
  assert_contains "bd pull remapped" "$(<"$bd_log")" "dolt pull|auto=0|socket=$socket_path|host=|port=|origin=$container_remote"
  assert_contains "bd push remapped" "$(<"$bd_log")" "dolt push|auto=0|socket=$socket_path|host=|port=|origin=$container_remote"
  assert_contains "bd remote restore" "$(<"$bd_log.origin")" "$host_remote"
}

test_sync_in_container() {
  require_command bd
  require_command dolt
  require_command nix
  require_command python3
  require_command jq

  local source_workspace="$TEST_TMP/sync-source-workspace"
  local staging_root="$TEST_TMP/sync-staging"
  local workspace="$TEST_TMP/sync-container-workspace"
  local entrypoint="$TEST_TMP/sync-entrypoint.sh"
  local stdout_path="$TEST_TMP/sync.out"
  local stderr_path="$TEST_TMP/sync.err"
  local socket_path="$TEST_TMP/sync-dolt.sock"
  local real_bd
  real_bd="$(real_bd_bin)"

  assert_launcher_exports_service_endpoint
  write_server_backed_beads "$source_workspace" "$socket_path" "$real_bd"
  mkdir -p "$staging_root" "$workspace/bin"
  run_stage_beads "$source_workspace" "$staging_root"
  cp -R "$staging_root/beads" "$workspace/.beads"
  write_fake_container_runtime_tools "$workspace/bin"
  ln -sf "$real_bd" "$workspace/bin/bd"
  rewrite_entrypoint_workspace "$REPO_ROOT/lib/sandbox/linux/entrypoint.sh" "$workspace" "$entrypoint"

  if ! BEADS_TEST_DOLT_SOCKET="$socket_path" \
    run_entrypoint_command "$entrypoint" "$workspace" "$stdout_path" "$stderr_path" \
      bash -c 'bd dolt pull && bd dolt push'; then
    fail "linux entrypoint failed for staged Dolt sync: $(<"$stderr_path")"
  fi

  assert_path_absent "$workspace/.beads/issues.jsonl"
}

test_no_jsonl_staged() {
  require_command nix
  require_command python3
  require_command jq

  local workspace="$TEST_TMP/stage-workspace"
  local staging_root="$TEST_TMP/staging"
  write_beads_files "$workspace" dolt
  mkdir -p "$staging_root"

  run_stage_beads "$workspace" "$staging_root"
  assert_path_exists "$staging_root/beads/config.yaml"
  assert_path_exists "$staging_root/beads/metadata.json"
  assert_path_absent "$staging_root/beads/issues.jsonl"

  assert_dolt_endpoint_failure linux "$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"
  assert_dolt_endpoint_failure darwin "$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh"
  assert_darwin_does_not_import_jsonl
}

ALL_TESTS=(
  test_fake_bd_contract
  test_bd_dolt_sync_uses_container_remote
  test_sync_in_container
  test_no_jsonl_staged
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
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
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
