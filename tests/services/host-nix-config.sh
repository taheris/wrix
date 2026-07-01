#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2031
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-host-nix.XXXXXX)"
SOCKET_PIDS=()
cleanup() {
  local pid
  for pid in "${SOCKET_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [[ -d "$TEST_TMP" ]]; then
    while IFS= read -r pid; do
      kill "$pid" 2>/dev/null || true
    done < <(find "$TEST_TMP" -name socket-pids -type f -exec cat {} + 2>/dev/null || true)
  fi
  if [[ -d "$TEST_TMP" ]]; then
    chmod -R u+w "$TEST_TMP"
    rm -rf "$TEST_TMP"
  fi
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

build_wrix() {
  cargo build --quiet -p wrix-cli --bin wrix
  printf '%s\n' "$REPO_ROOT/target/debug/wrix"
}

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

eval_expr_json() {
  local expr="$1"
  local system
  system="$(resolve_system)"
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\"$system\".lib;
    in $expr
  "
}

write_fake_runtime() {
  local runtime="$1"
  cat >"$runtime" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

STATE_DIR="${WRIX_FAKE_RUNTIME_STATE:?}"
mkdir -p "$STATE_DIR"

state_file() {
  local name="$1"
  printf '%s/%s.state\n' "$STATE_DIR" "$name"
}

run_file() {
  local name="$1"
  printf '%s/run-%s\n' "$STATE_DIR" "$name"
}

port_lines_for_args() {
  local run_args="$1"
  local previous=""
  local token
  local mapping
  local rest
  local host_port
  local container_port
  local -a tokens=()
  read -r -a tokens <<<"$run_args"
  for token in "${tokens[@]}"; do
    if [[ "$previous" == "-p" || "$previous" == "--publish" ]]; then
      mapping="$token"
      case "$mapping" in
        127.0.0.1:*:*)
          rest="${mapping#127.0.0.1:}"
          host_port="${rest%%:*}"
          container_port="${rest##*:}"
          printf '%s/tcp -> 127.0.0.1:%s\n' "$container_port" "$host_port"
          ;;
      esac
    fi
    previous="$token"
  done
}

socket_path_for_args() {
  local run_args="$1"
  local previous=""
  local token
  local source
  local -a tokens=()
  read -r -a tokens <<<"$run_args"
  for token in "${tokens[@]}"; do
    if [[ "$previous" == "-v" && "$token" == *":/run/wrix:rw" ]]; then
      source="${token%:/run/wrix:rw}"
      printf '%s/dolt.sock\n' "$source"
      return 0
    fi
    if [[ "$previous" == "--publish-socket" && "$token" == *":/run/wrix/dolt.sock" ]]; then
      printf '%s\n' "${token%:/run/wrix/dolt.sock}"
      return 0
    fi
    previous="$token"
  done
  return 1
}

start_socket_listener() {
  local socket_path="$1"
  mkdir -p "$(dirname "$socket_path")"
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
  printf '%s\n' "$!" >>"$STATE_DIR/socket-pids"
}

case "${1:-}" in
  container)
    if [[ "${2:-}" == "exists" ]]; then
      [[ -f "$(state_file "${3:-}")" ]]
      exit $?
    fi
    ;;
  inspect)
    if [[ -f "$(state_file "${@: -1}")" ]]; then
      printf 'true\n'
      exit 0
    fi
    exit 1
    ;;
  ps)
    ;;
  run)
    name=""
    previous=""
    for arg in "$@"; do
      if [[ "$previous" == "--name" ]]; then
        name="$arg"
      fi
      previous="$arg"
    done
    [[ -n "$name" ]]
    printf 'running\n' >"$(state_file "$name")"
    printf '%s\n' "$*" >"$(run_file "$name")"
    if socket_path="$(socket_path_for_args "$*")"; then
      start_socket_listener "$socket_path"
    fi
    ;;
  port)
    name="${2:-}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      printf 'Error: no such object: "%s"\n' "$name" >&2
      exit 1
    fi
    run_path="$(run_file "$name")"
    if [[ -f "$run_path" ]]; then
      port_lines_for_args "$(<"$run_path")"
    fi
    ;;
  rm)
    name="${@: -1}"
    rm -f "$(state_file "$name")" "$(run_file "$name")"
    ;;
  logs)
    printf 'logs\n'
    ;;
  *)
    printf 'unsupported fake runtime command: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$runtime"
}

write_fake_nix_store() {
  local nix_store="$1"
  cat >"$nix_store" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case "$*" in
  --generate-binary-cache-key*)
    key_name="$2"
    secret_path="$3"
    public_path="$4"
    printf '%s-secret\n' "$key_name" >"$secret_path"
    printf '%s:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n' "$key_name" >"$public_path"
    ;;
  --add-fixed*)
    src="${@: -1}"
    store_root="${WRIX_FAKE_NIX_STORE_ROOT:?}"
    out="$store_root/00000000000000000000000000000000-wrix-cache-hook-wrapper"
    rm -rf "$out"
    mkdir -p "$store_root"
    cp -R "$src" "$out"
    chmod -R a-w "$out"
    printf '%s\n' "$out"
    ;;
  *)
    printf 'unsupported fake nix-store command: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$nix_store"
}

write_fake_nix() {
  local nix="$1"
  cat >"$nix" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "config" && "${2:-}" == "show" ]]; then
  while IFS= read -r line; do
    case "${WRIX_FAKE_NIX_IGNORE:-}" in
      substituter)
        [[ "$line" == extra-substituters* ]] && continue
        ;;
      key)
        [[ "$line" == extra-trusted-public-keys* ]] && continue
        ;;
      builders)
        [[ "$line" == builders-use-substitutes* ]] && continue
        ;;
      hook)
        [[ "$line" == post-build-hook* ]] && continue
        ;;
    esac
    printf '%s\n' "$line"
  done <<<"${NIX_CONFIG:-}"
  exit 0
fi
printf 'unsupported fake nix command: %s\n' "$*" >&2
exit 2
EOF
  chmod +x "$nix"
}

write_fake_systemctl() {
  local systemctl="$1"
  cat >"$systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 1
EOF
  chmod +x "$systemctl"
}

with_fake_tools() {
  local bin_dir="$TEST_TMP/bin"
  mkdir -p "$bin_dir" "$TEST_TMP/runtime-state" "$TEST_TMP/fake-store"
  write_fake_runtime "$bin_dir/podman"
  write_fake_nix_store "$bin_dir/nix-store"
  write_fake_nix "$bin_dir/nix"
  write_fake_systemctl "$bin_dir/systemctl"
  export PATH="$bin_dir:$PATH"
  export WRIX_CONTAINER_RUNTIME="$bin_dir/podman"
  export WRIX_FAKE_RUNTIME_STATE="$TEST_TMP/runtime-state"
  export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
  export WRIX_FAKE_NIX_STORE_ROOT="$TEST_TMP/fake-store"
  export WRIX_NIX_STORE="$bin_dir/nix-store"
  export WRIX_NIX_STORE_BIN="$bin_dir/nix-store"
  export WRIX_NIX_BIN="$bin_dir/nix"
  unset NIX_CONFIG WRIX_FAKE_NIX_IGNORE WRIX_SERVICE_IMAGE WRIX_SERVICE_IMAGE_SOURCE
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label missing '$needle'"
    return 1
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label unexpectedly contains '$needle'"
    return 1
  fi
}

json_get() {
  local file="$1"
  local path="$2"
  python3 - "$file" "$path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)
for part in sys.argv[2].split('.'):
    value = value[part]
print(value)
PY
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

test_mkdevshell_nix_cache() {
  if ! command -v nix >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
    exit 77
  fi
  local result result_file default_hook disabled_hook custom_env
  local default_hook_file disabled_hook_file wrix_bin workspace output nix_config stderr_file suppressed_stderr saved_path
  local disabled_workspace socket_path fake_wrix args_log disabled_output disabled_stderr
  if ! result="$(eval_expr_json '
    let
      defaultShell = lib.mkDevShell { profile = lib.profiles.base; };
      disabledShell = lib.mkDevShell { profile = lib.profiles.base; nixCache = false; };
      customShell = lib.mkDevShell {
        profile = lib.profiles.base;
        nixCache = {
          enable = true;
          requireTrustedNix = false;
          publish = {
            packages = false;
            checks = false;
            devShell = false;
            includeRoots = [ ".#packages.custom" ".#checks.extra" ];
            excludeRoots = [ ".#packages.skip" ];
          };
          warm = {
            packages = false;
            checks = true;
            devShell = false;
            includeRoots = [ ".#devShells.custom" ];
            excludeRoots = [ ".#checks.skip" ];
          };
          warnSize = "1G";
          pendingTtl = "2d";
          pruneInterval = "3h";
        };
      };
    in {
      defaultHook = defaultShell.shellHook;
      disabledHook = disabledShell.shellHook;
      customEnv = {
        inherit (customShell)
          WRIX_NIX_CACHE_REQUIRE_TRUSTED
          WRIX_CACHE_PUBLISH_PACKAGES
          WRIX_CACHE_PUBLISH_CHECKS
          WRIX_CACHE_PUBLISH_DEVSHELL
          WRIX_CACHE_PUBLISH_INCLUDE
          WRIX_CACHE_PUBLISH_EXCLUDE
          WRIX_CACHE_WARM_PACKAGES
          WRIX_CACHE_WARM_CHECKS
          WRIX_CACHE_WARM_DEVSHELL
          WRIX_CACHE_WARM_INCLUDE
          WRIX_CACHE_WARM_EXCLUDE
          WRIX_CACHE_SOFT_LIMIT_BYTES
          WRIX_CACHE_PENDING_RETENTION_SECS
          WRIX_CACHE_PRUNE_INTERVAL_SECS
          ;
      };
    }
  ')"; then
    fail "mkDevShell nixCache evaluation failed"
    return 1
  fi
  result_file="$TEST_TMP/mkdevshell.json"
  default_hook_file="$TEST_TMP/mkdevshell-default-hook.sh"
  disabled_hook_file="$TEST_TMP/mkdevshell-disabled-hook.sh"
  printf '%s\n' "$result" >"$result_file"
  default_hook="$(json_get "$result_file" defaultHook)"
  disabled_hook="$(json_get "$result_file" disabledHook)"
  custom_env="$(json_get "$result_file" customEnv.WRIX_CACHE_PUBLISH_INCLUDE)"
  sed -E "s|/nix/store/[[:alnum:]]+-wrix-host-nix-config\.sh|$REPO_ROOT/lib/services/host-nix-config.sh|g" <<<"$default_hook" >"$default_hook_file"
  printf '%s\n' "$disabled_hook" >"$disabled_hook_file"

  assert_contains "default mkDevShell hook" "$default_hook" "service start" || return 1
  assert_contains "default mkDevShell hook" "$default_hook" "WRIX_HOST_NIX_CONFIG_PRINT=1" || return 1
  assert_contains "default mkDevShell hook" "$default_hook" "export NIX_CONFIG" || return 1
  assert_not_contains "default mkDevShell hook" "$default_hook" "service start --no-cache" || return 1
  assert_contains "disabled mkDevShell hook" "$disabled_hook" '.beads/dolt' || return 1
  assert_contains "disabled mkDevShell hook" "$disabled_hook" "service start --no-cache" || return 1
  assert_not_contains "disabled mkDevShell hook" "$disabled_hook" "WRIX_HOST_NIX_CONFIG_PRINT=1" || return 1
  assert_contains "custom publish include" "$custom_env" ".#packages.custom" || return 1
  assert_contains "custom publish include" "$custom_env" ".#checks.extra" || return 1
  [[ "$(json_get "$result_file" customEnv.WRIX_NIX_CACHE_REQUIRE_TRUSTED)" == "0" ]] || { fail "requireTrustedNix did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PUBLISH_PACKAGES)" == "0" ]] || { fail "publish.packages did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PUBLISH_CHECKS)" == "0" ]] || { fail "publish.checks did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PUBLISH_DEVSHELL)" == "0" ]] || { fail "publish.devShell did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PUBLISH_EXCLUDE)" == ".#packages.skip" ]] || { fail "publish.excludeRoots did not map"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_WARM_PACKAGES)" == "0" ]] || { fail "warm.packages did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_WARM_CHECKS)" == "1" ]] || { fail "warm.checks did not map to 1"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_WARM_DEVSHELL)" == "0" ]] || { fail "warm.devShell did not map to 0"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_WARM_INCLUDE)" == ".#devShells.custom" ]] || { fail "warm.includeRoots did not map"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_WARM_EXCLUDE)" == ".#checks.skip" ]] || { fail "warm.excludeRoots did not map"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_SOFT_LIMIT_BYTES)" == "1073741824" ]] || { fail "warnSize did not map to bytes"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PENDING_RETENTION_SECS)" == "172800" ]] || { fail "pendingTtl did not map to seconds"; return 1; }
  [[ "$(json_get "$result_file" customEnv.WRIX_CACHE_PRUNE_INTERVAL_SECS)" == "10800" ]] || { fail "pruneInterval did not map to seconds"; return 1; }

  wrix_bin="$(build_wrix)"
  saved_path="$PATH"
  with_fake_tools
  export HOME="$TEST_TMP/home-mkdevshell"
  export XDG_STATE_HOME="$TEST_TMP/state-mkdevshell"
  export XDG_CACHE_HOME="$TEST_TMP/cache-mkdevshell"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/mkdevshell-cache-workspace"
  stderr_file="$TEST_TMP/mkdevshell-cache.err"
  mkdir -p "$workspace"

  if ! output="$(
    (
      cd "$workspace"
      # shellcheck source=/dev/null
      WRIX_BIN="$wrix_bin" . "$default_hook_file" >/dev/null
      printf '%s\n' "$NIX_CONFIG"
    ) 2>"$stderr_file"
  )"; then
    fail "default mkDevShell hook failed: $(cat "$stderr_file")"
    return 1
  fi
  nix_config="$output"
  assert_contains "sourced NIX_CONFIG" "$nix_config" "extra-substituters = file://" || return 1
  assert_contains "sourced NIX_CONFIG" "$nix_config" "extra-trusted-public-keys = " || return 1
  assert_contains "sourced NIX_CONFIG" "$nix_config" "builders-use-substitutes = true" || return 1
  assert_contains "sourced NIX_CONFIG" "$nix_config" "post-build-hook = $TEST_TMP/fake-store/" || return 1
  assert_contains "mkDevShell cache reminder" "$(cat "$stderr_file")" "publish manifest is empty" || return 1
  if ! "$WRIX_CONTAINER_RUNTIME" container exists mkdevshell-cache-workspace-service; then
    fail "default mkDevShell hook did not start the workspace service"
    return 1
  fi

  suppressed_stderr="$TEST_TMP/mkdevshell-cache-suppressed.err"
  if ! (
    cd "$workspace"
    export WRIX_NIX_CACHE_REMINDER=0
    # shellcheck source=/dev/null
    WRIX_BIN="$wrix_bin" . "$default_hook_file" >/dev/null
  ) 2>"$suppressed_stderr"; then
    fail "default mkDevShell hook with suppressed reminder failed: $(cat "$suppressed_stderr")"
    return 1
  fi
  assert_not_contains "suppressed reminder stderr" "$(cat "$suppressed_stderr")" "publish manifest is empty" || return 1

  disabled_workspace="$TEST_TMP/mkdevshell-beads-workspace"
  socket_path="$TEST_TMP/mkdevshell-beads.sock"
  fake_wrix="$TEST_TMP/fake-wrix-beads"
  args_log="$TEST_TMP/fake-wrix-beads.args"
  disabled_stderr="$TEST_TMP/mkdevshell-disabled.err"
  : > "$args_log"
  mkdir -p "$disabled_workspace/.beads/dolt"
  start_unix_socket "$socket_path"
  cat >"$fake_wrix" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "\$*" >> "$args_log"
if [[ "\${1:-}" == "service" && "\${2:-}" == "start" && "\${3:-}" == "--no-cache" ]]; then
  exit 0
fi
if [[ "\${1:-}" == "service" && "\${2:-}" == "endpoints" && "\${3:-}" == "--no-cache" ]]; then
  printf '%s\n' '{"endpoints":{"dolt":{"transport":"unix","socket":"$socket_path"}}}'
  exit 0
fi
if [[ "\${1:-}" == "service" && "\${2:-}" == "dolt" && "\${3:-}" == "wait" ]]; then
  exit 0
fi
printf 'fake wrix beads: unexpected args: %s\n' "\$*" >&2
exit 2
SCRIPT
  chmod +x "$fake_wrix"
  if ! disabled_output="$(
    (
      cd "$disabled_workspace"
      unset NIX_CONFIG BEADS_DOLT_SERVER_SOCKET BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
      # shellcheck source=/dev/null
      WRIX_BIN="$fake_wrix" . "$disabled_hook_file" >/dev/null
      printf 'NIX_CONFIG=%s\n' "${NIX_CONFIG:-}"
      printf 'BEADS_DOLT_SERVER_SOCKET=%s\n' "${BEADS_DOLT_SERVER_SOCKET:-}"
    ) 2>"$disabled_stderr"
  )"; then
    fail "disabled mkDevShell hook failed: $(cat "$disabled_stderr")"
    return 1
  fi
  assert_contains "nixCache=false beads socket" "$disabled_output" "BEADS_DOLT_SERVER_SOCKET=$socket_path" || return 1
  assert_not_contains "nixCache=false output" "$disabled_output" "extra-substituters" || return 1
  assert_contains "nixCache=false wrix calls" "$(cat "$args_log")" "service start --no-cache" || return 1
  assert_contains "nixCache=false wrix calls" "$(cat "$args_log")" "service endpoints --no-cache" || return 1
  assert_contains "nixCache=false wrix calls" "$(cat "$args_log")" "service dolt wait" || return 1
  PATH="$saved_path"
  export PATH
}

test_mkdevshell_beads_workspace_does_not_run_raw_dolt() {
  if ! command -v nix >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
    exit 77
  fi
  local result result_file hook_file hook wrix_bin workspace stderr_file output saved_path
  local socket_path raw_dolt_log
  if ! result="$(eval_expr_json '
    let
      shell = lib.mkDevShell { profile = lib.profiles.base; };
    in { hook = shell.shellHook; }
  ')"; then
    fail "mkDevShell hook evaluation failed"
    return 1
  fi
  result_file="$TEST_TMP/mkdevshell-beads-cache.json"
  hook_file="$TEST_TMP/mkdevshell-beads-cache-hook.sh"
  printf '%s\n' "$result" >"$result_file"
  hook="$(json_get "$result_file" hook)"
  sed -E "s|/nix/store/[[:alnum:]]+-wrix-host-nix-config\.sh|$REPO_ROOT/lib/services/host-nix-config.sh|g" <<<"$hook" >"$hook_file"

  wrix_bin="$(build_wrix)"
  saved_path="$PATH"
  with_fake_tools
  raw_dolt_log="$TEST_TMP/raw-dolt.log"
  cat >"$TEST_TMP/bin/dolt" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
printf 'raw dolt invoked: %s\n' "\$*" >> "$raw_dolt_log"
printf 'raw dolt should not run during mkDevShell entry\n' >&2
exit 2
SCRIPT
  chmod +x "$TEST_TMP/bin/dolt"
  export HOME="$TEST_TMP/home-mkdevshell-beads-cache"
  export XDG_STATE_HOME="$TEST_TMP/state-mkdevshell-beads-cache"
  export XDG_CACHE_HOME="$TEST_TMP/cache-mkdevshell-beads-cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/mkdevshell-beads-cache-workspace"
  stderr_file="$TEST_TMP/mkdevshell-beads-cache.err"
  mkdir -p \
    "$workspace/.beads/dolt/beads/.dolt" \
    "$workspace/.git/beads-worktrees/beads/.beads/dolt-remote" \
    "$workspace/.wrix"
  socket_path="$workspace/.wrix/dolt.sock"
  start_unix_socket "$socket_path"

  if ! output="$(
    (
      cd "$workspace"
      unset BEADS_DOLT_SERVER_SOCKET BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
      # shellcheck source=/dev/null
      WRIX_BIN="$wrix_bin" . "$hook_file" >/dev/null
      printf 'BEADS_DOLT_SERVER_SOCKET=%s\n' "${BEADS_DOLT_SERVER_SOCKET:-}"
    ) 2>"$stderr_file"
  )"; then
    fail "mkDevShell hook failed in beads workspace: $(cat "$stderr_file")"
    return 1
  fi
  assert_contains "beads cache hook socket" "$output" "BEADS_DOLT_SERVER_SOCKET=$socket_path" || return 1
  if [[ -s "$raw_dolt_log" ]]; then
    fail "mkDevShell invoked raw dolt: $(cat "$raw_dolt_log")"
    return 1
  fi
  assert_not_contains "beads cache hook stderr" "$(cat "$stderr_file")" "raw dolt should not run" || return 1
  PATH="$saved_path"
  export PATH
}

test_mkdevshell_loom_internal_worktree_uses_repo_service() {
  if ! command -v nix >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
    exit 77
  fi
  local result result_file hook_file hook workspace repo wrix_bin output
  if ! result="$(eval_expr_json '
    let
      shell = lib.mkDevShell { profile = lib.profiles.base; };
    in { hook = shell.shellHook; }
  ')"; then
    fail "mkDevShell hook evaluation failed"
    return 1
  fi
  result_file="$TEST_TMP/loom-integration-shell.json"
  hook_file="$TEST_TMP/loom-integration-shell-hook.sh"
  repo="$TEST_TMP/loom-integration-repo"
  workspace="$repo/.loom/integration"
  printf '%s\n' "$result" >"$result_file"
  hook="$(json_get "$result_file" hook)"
  printf '%s\n' "$hook" >"$hook_file"
  mkdir -p "$repo/.git" "$workspace"
  wrix_bin="$(build_wrix)"
  with_fake_tools
  export HOME="$TEST_TMP/home-loom-integration"
  export XDG_STATE_HOME="$TEST_TMP/state-loom-integration"
  export XDG_CACHE_HOME="$TEST_TMP/cache-loom-integration"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  if ! output="$(
    (
      cd "$workspace"
      export WRIX_BIN="$wrix_bin"
      # shellcheck source=/dev/null
      . "$hook_file"
    ) 2>&1
  )"; then
    fail "mkDevShell hook failed in .loom/integration: $output"
    return 1
  fi
  assert_contains "mkDevShell output" "$output" "Wrix development shell" || return 1
  if ! "$WRIX_CONTAINER_RUNTIME" container exists loom-integration-repo-service; then
    fail "mkDevShell did not start the repository service from .loom/integration"
    return 1
  fi
  if "$WRIX_CONTAINER_RUNTIME" container exists integration-service; then
    fail "mkDevShell started an internal .loom service container"
    return 1
  fi
}

test_host_nix_configures_cache_and_hook() {
  if ! command -v python3 >/dev/null 2>&1; then
    exit 77
  fi
  local wrix_bin workspace output endpoints state_root cache_root public_key hook_path wrapper
  wrix_bin="$(build_wrix)"
  with_fake_tools
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/endpoints.json")
  state_root="$(json_get "$TEST_TMP/endpoints.json" state_root)"
  cache_root="$(json_get "$TEST_TMP/endpoints.json" cache_root)"
  public_key="$(tr -d '\n' <"$state_root/keys/cache.pub")"

  output="$(
    (
      cd "$workspace"
      WRIX_SERVICE_BIN="$wrix_bin" \
        WRIX_CACHE_HOOK_BIN="$REPO_ROOT/target/debug/wrix-cache-hook" \
        WRIX_CACHE_PUBLISH_BIN="$REPO_ROOT/target/debug/wrix-cache-publish" \
        WRIX_HOST_NIX_CONFIG_PRINT=1 \
        . "$REPO_ROOT/lib/services/host-nix-config.sh"
    ) 2>"$TEST_TMP/reminder.err"
  )"
  assert_contains "NIX_CONFIG" "$output" "extra-substituters = file://$cache_root" || return 1
  assert_contains "NIX_CONFIG" "$output" "extra-trusted-public-keys = $public_key" || return 1
  assert_contains "NIX_CONFIG" "$output" "builders-use-substitutes = true" || return 1
  assert_contains "NIX_CONFIG" "$output" "post-build-hook = $TEST_TMP/fake-store/" || return 1

  hook_path="$(printf '%s\n' "$output" | awk -F ' = ' '/post-build-hook/{print $2}')"
  wrapper="$hook_path"
  [[ -x "$wrapper" ]] || { fail "stored hook wrapper is not executable: $wrapper"; return 1; }
  endpoints="$(cat "$TEST_TMP/endpoints.json")"
  assert_contains "wrapper workspace hash" "$(cat "$wrapper")" "$(json_get "$TEST_TMP/endpoints.json" workspace_hash)" || return 1
  assert_contains "wrapper owner uid" "$(cat "$wrapper")" "--owner-uid \"$(id -u)\"" || return 1
  assert_contains "wrapper owner gid" "$(cat "$wrapper")" "--owner-gid \"$(id -g)\"" || return 1
  assert_contains "wrapper state root" "$(cat "$wrapper")" "$state_root" || return 1
  assert_contains "wrapper cache root" "$(cat "$wrapper")" "$cache_root" || return 1
  assert_contains "wrapper manifest" "$(cat "$wrapper")" "$state_root/publish-roots.json" || return 1
  assert_contains "wrapper publisher" "$(cat "$wrapper")" "$REPO_ROOT/target/debug/wrix-cache-publish" || return 1
  assert_contains "endpoint metadata" "$endpoints" "$cache_root" || return 1
}

test_host_nix_config_fails_when_trusted_setting_ignored() {
  if ! command -v python3 >/dev/null 2>&1; then
    exit 77
  fi
  local wrix_bin workspace
  wrix_bin="$(build_wrix)"
  with_fake_tools
  export HOME="$TEST_TMP/home-ignored"
  export XDG_STATE_HOME="$TEST_TMP/state-ignored"
  export XDG_CACHE_HOME="$TEST_TMP/cache-ignored"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/workspace-ignored"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/start-ignored.txt")
  if (
    cd "$workspace"
    export WRIX_FAKE_NIX_IGNORE=hook
    WRIX_SERVICE_BIN="$wrix_bin" . "$REPO_ROOT/lib/services/host-nix-config.sh"
  ) >"$TEST_TMP/ignored.out" 2>"$TEST_TMP/ignored.err"; then
    fail "host config succeeded even though fake Nix ignored the hook"
    return 1
  fi
  assert_contains "ignored error" "$(cat "$TEST_TMP/ignored.err")" "host Nix does not honor post-build-hook" || return 1
}

test_host_nix_config_rejects_non_wrix_hook() {
  local wrix_bin workspace
  wrix_bin="$(build_wrix)"
  with_fake_tools
  export HOME="$TEST_TMP/home-conflict"
  export XDG_STATE_HOME="$TEST_TMP/state-conflict"
  export XDG_CACHE_HOME="$TEST_TMP/cache-conflict"
  export NIX_CONFIG="post-build-hook = /tmp/other-hook"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/workspace-conflict"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/start-conflict.txt")
  if (
    cd "$workspace"
    WRIX_SERVICE_BIN="$wrix_bin" . "$REPO_ROOT/lib/services/host-nix-config.sh"
  ) >"$TEST_TMP/conflict.out" 2>"$TEST_TMP/conflict.err"; then
    fail "host config accepted a non-wrix hook"
    return 1
  fi
  assert_contains "conflict error" "$(cat "$TEST_TMP/conflict.err")" "existing non-wrix post-build-hook conflicts" || return 1
}

ALL_TESTS=(
  test_mkdevshell_nix_cache
  test_mkdevshell_beads_workspace_does_not_run_raw_dolt
  test_mkdevshell_loom_internal_worktree_uses_repo_service
  test_host_nix_configures_cache_and_hook
  test_host_nix_config_fails_when_trusted_setting_ignored
  test_host_nix_config_rejects_non_wrix_hook
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
