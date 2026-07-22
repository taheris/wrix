#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

TEST_TMP=""
LAUNCHER=""
IMAGE_REF=""
LISTENER_PID=""
WORKSPACE=""

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

darwin_vmnet_gateway() {
  local network_json
  network_json=$(container network inspect default)
  printf '%s\n' "$network_json" | jq -er '
    (if type == "array" then .[0] else . end)
    | .status.ipv4Gateway
    | strings
    | select(test("^[0-9]+(\\.[0-9]+){3}$"))
  '
}

start_unrelated_listener() {
  local pid_output="$1"
  local target_output="$2"
  local port_output="$3"
  local target bind_host host_probe port_file server_script server_log server_pid port attempt

  case "$(uname -s)" in
    Darwin)
      target=$(darwin_vmnet_gateway)
      bind_host="$target"
      host_probe="$target"
      ;;
    Linux)
      target="169.254.1.2"
      bind_host="127.0.0.1"
      host_probe="$bind_host"
      ;;
    *)
      fail "unsupported cache-network host: $(uname -s)"
      return 1
      ;;
  esac

  port_file="$TEST_TMP/unrelated.port"
  server_script="$TEST_TMP/unrelated.py"
  server_log="$TEST_TMP/unrelated.log"
  cat >"$server_script" <<'PYTHON'
import pathlib
import socket
import sys

host = sys.argv[1]
port_file = pathlib.Path(sys.argv[2])
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as server:
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((host, 0))
    server.listen()
    port_file.write_text(f"{server.getsockname()[1]}\n", encoding="utf-8")
    while True:
        connection, _address = server.accept()
        with connection:
            connection.recv(4096)
            connection.sendall(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
PYTHON
  python3 "$server_script" "$bind_host" "$port_file" >"$server_log" 2>&1 &
  server_pid=$!
  printf -v "$pid_output" '%s' "$server_pid"
  printf -v "$target_output" '%s' "$target"

  for ((attempt = 1; attempt <= 50; attempt++)); do
    if [[ -s "$port_file" ]]; then
      break
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
      cat "$server_log" >&2
      fail "unrelated host listener exited before accepting connections"
      return 1
    fi
    sleep 0.1
  done
  [[ -s "$port_file" ]] || {
    fail "unrelated host listener did not publish its port"
    return 1
  }
  port=$(<"$port_file")
  [[ "$port" =~ ^[0-9]+$ ]] || {
    fail "unrelated host listener published an invalid port: $port"
    return 1
  }
  printf -v "$port_output" '%s' "$port"

  if ! curl --noproxy '*' --fail --silent --show-error --connect-timeout 2 --max-time 5 \
    "http://$host_probe:$port/" >/dev/null; then
    cat "$server_log" >&2
    fail "host could not reach the unrelated listener"
    return 1
  fi
}

configure_service_image() {
  local service_digest_path

  export WRIX_SERVICE_IMAGE_SOURCE
  WRIX_SERVICE_IMAGE_SOURCE=$(nix build --no-link --print-out-paths --no-warn-dirty \
    .#wrix-service-image.source)
  export WRIX_SERVICE_IMAGE
  WRIX_SERVICE_IMAGE=$(nix eval --raw --no-warn-dirty .#wrix-service-image.ref)
  export WRIX_SERVICE_IMAGE_SOURCE_KIND
  WRIX_SERVICE_IMAGE_SOURCE_KIND=$(nix eval --raw --no-warn-dirty \
    .#wrix-service-image.source_kind)
  service_digest_path=$(nix eval --raw --no-warn-dirty .#wrix-service-image.digest)
  [[ -f "$service_digest_path" ]] || {
    fail "service image digest was not realized: $service_digest_path"
    return 1
  }
  export WRIX_SERVICE_IMAGE_DIGEST
  WRIX_SERVICE_IMAGE_DIGEST=$(<"$service_digest_path")
}

cleanup() {
  local status="$?"
  local wait_status=0
  trap - EXIT

  if [[ -n "$LAUNCHER" && -d "$WORKSPACE" ]]; then
    if ! (
      cd "$WORKSPACE"
      HOME="$TEST_TMP/home" XDG_STATE_HOME="$TEST_TMP/state" XDG_CACHE_HOME="$TEST_TMP/cache" \
        WRIX_SERVICE_ALLOW_TEMP_CACHE=1 "$LAUNCHER/bin/wrix" service stop
    ); then
      printf 'WARN: could not stop the cache-network service container\n' >&2
    fi
  fi
  if [[ -n "$LISTENER_PID" ]] && kill -0 "$LISTENER_PID" 2>/dev/null; then
    if kill "$LISTENER_PID"; then
      set +e
      wait "$LISTENER_PID"
      wait_status=$?
      set -e
      if [[ "$wait_status" -ne 0 && "$wait_status" -ne 143 ]]; then
        printf 'WARN: unrelated host listener exited with status %s\n' "$wait_status" >&2
      fi
    else
      printf 'WARN: could not stop unrelated host listener %s\n' "$LISTENER_PID" >&2
    fi
  fi
  if [[ -n "$IMAGE_REF" ]] && ! wrix_remove_image_ref "$IMAGE_REF"; then
    printf 'WARN: could not remove test image %s\n' "$IMAGE_REF" >&2
  fi
  if [[ -n "$TEST_TMP" ]]; then
    rm -rf "$TEST_TMP"
  fi
  exit "$status"
}

test_limit_mode_cache_endpoint() {
  local profile_config profile_config_tmp spawn_config deploy_key target port probe output

  wrix_require_live_sandbox
  command -v curl >/dev/null 2>&1 || wrix_live_skip "curl not on PATH"
  command -v python3 >/dev/null 2>&1 || wrix_live_skip "python3 not on PATH"
  cd "$REPO_ROOT"

  TEST_TMP=$(mktemp -d -t wrix-cache-network-live.XXXXXX)
  trap cleanup EXIT
  WORKSPACE="$TEST_TMP/workspace"
  profile_config="$TEST_TMP/profile.json"
  profile_config_tmp="$TEST_TMP/profile.tmp.json"
  spawn_config="$TEST_TMP/spawn.json"
  deploy_key="$TEST_TMP/deploy-key"
  output="$TEST_TMP/spawn.out"
  mkdir -p "$WORKSPACE" "$TEST_TMP/home" "$TEST_TMP/state" "$TEST_TMP/cache"
  git -C "$WORKSPACE" init -q -b main

  LAUNCHER=$(wrix_build_live_launcher)
  configure_service_image
  IMAGE_REF=$(wrix_live_image_ref "cache-network-live-$$")
  wrix_write_profile_config "$profile_config" "$IMAGE_REF" \
    "$(wrix_realize_test_image_source claude)" claude
  jq '.services.nix_cache.enable = true' "$profile_config" >"$profile_config_tmp"
  mv "$profile_config_tmp" "$profile_config"
  wrix_make_ed25519_key "$deploy_key" "cache-network-live-test"
  start_unrelated_listener LISTENER_PID target port

  probe=$(cat <<PROBE
set -euo pipefail
cache_url=\$(awk -F' = ' '\$1 == "extra-substituters" { print \$2; exit }' <<<"\$NIX_CONFIG")
[[ "\$cache_url" =~ ^http://[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+:[0-9]+$ ]]
curl --noproxy '*' --fail --silent --show-error --connect-timeout 5 --max-time 10 "\$cache_url/nix-cache-info" | grep -F 'WantMassQuery: 1' >/dev/null
if curl --noproxy '*' --fail --silent --show-error --connect-timeout 2 --max-time 5 http://$target:$port/ >/tmp/wrix-unrelated-service 2>&1; then
  echo 'unrelated host listener reachable through cache endpoint exception' >&2
  exit 1
fi
PROBE
)
  wrix_write_spawn_config "$spawn_config" "$WORKSPACE" bash -lc "$probe"

  if ! HOME="$TEST_TMP/home" XDG_STATE_HOME="$TEST_TMP/state" XDG_CACHE_HOME="$TEST_TMP/cache" \
    WRIX_SERVICE_ALLOW_TEMP_CACHE=1 WRIX_DEPLOY_KEY="$deploy_key" WRIX_GIT_SIGN=0 \
    WRIX_NETWORK=limit \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$output" 2>&1; then
    cat "$output" >&2
    fail "assembled limit-mode sandbox could not isolate the live project cache endpoint"
    return 1
  fi

  printf 'PASS: assembled limit-mode sandbox reaches only its live project cache endpoint\n'
}

if [[ "$#" -eq 0 ]]; then
  test_limit_mode_cache_endpoint
else
  test_name="$1"
  if ! declare -f "$test_name" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$test_name" >&2
    exit 1
  fi
  "$test_name"
fi
