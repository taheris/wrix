#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_ENTRYPOINT="$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"
DARWIN_ENTRYPOINT="$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh"
BASH_BIN="${BASH:-$(command -v bash)}"
AWK_BIN="$(command -v awk)"
SORT_BIN="$(command -v sort)"
GREP_BIN="$(command -v grep)"

TEST_TMP="$(mktemp -d -t wrix-services-sandbox-nix.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

WRIX_PACKAGE=""

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP: python3 is required for JSON assertions\n' >&2
    exit 77
  fi
}

build_wrix_package() {
  if [[ -n "$WRIX_PACKAGE" ]]; then
    printf '%s\n' "$WRIX_PACKAGE"
    return 0
  fi
  if ! command -v nix >/dev/null 2>&1; then
    printf 'SKIP: nix is required to build the launcher wrapper\n' >&2
    exit 77
  fi
  local out_link="$TEST_TMP/wrix-package"
  nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#wrix" >/dev/null
  WRIX_PACKAGE="$out_link"
  printf '%s\n' "$WRIX_PACKAGE"
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

log_call() {
  printf '%s\n' "$*" >>"$STATE_DIR/calls"
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
    if [[ -z "$name" ]]; then
      printf 'missing --name\n' >&2
      exit 2
    fi
    printf 'running\n' >"$(state_file "$name")"
    printf '%s\n' "$*" >"$STATE_DIR/run-$name"
    log_call "$*"
    ;;
  rm)
    rm -f "$(state_file "${@: -1}")"
    log_call "$*"
    ;;
  logs)
    printf 'logs for %s\n' "${2:-}"
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
  *)
    printf 'unsupported fake nix-store command: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$nix_store"
}

with_fake_service_env() {
  local name="$1"
  local bin_dir="$TEST_TMP/$name/bin"
  local runtime_state="$TEST_TMP/$name/runtime-state"
  local home="$TEST_TMP/$name/home"
  mkdir -p "$bin_dir" "$runtime_state" "$home"
  write_fake_runtime "$bin_dir/podman"
  write_fake_nix_store "$bin_dir/nix-store"
  export HOME="$home"
  export XDG_STATE_HOME="$TEST_TMP/$name/state"
  export XDG_CACHE_HOME="$TEST_TMP/$name/cache"
  export WRIX_CONTAINER_RUNTIME="$bin_dir/podman"
  export WRIX_FAKE_RUNTIME_STATE="$runtime_state"
  export WRIX_NIX_STORE="$bin_dir/nix-store"
  export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
  mkdir -p "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
}

write_profile_config() {
  local path="$1"
  cat >"$path" <<'JSON'
{
  "schema": 1,
  "system": "test",
  "profile": {
    "name": "base",
    "env": {},
    "mounts": [],
    "writable_dirs": [],
    "network_allowlist": []
  },
  "image": {
    "ref": "localhost/wrix-test:latest",
    "source": "/nix/store/fake-image",
    "digest": "sha256:test"
  },
  "agent": {
    "kind": "direct"
  },
  "resources": {
    "cpus": null,
    "memory_mb": 4096,
    "pids_limit": 4096
  },
  "security": {
    "deploy_key": null
  },
  "network": {
    "default_mode": "open",
    "ipv6": "disabled"
  },
  "services": {
    "beads": {
      "enable": "auto"
    },
    "nix_cache": {
      "enable": true
    }
  },
  "features": {
    "mcp_runtime": false
  }
}
JSON
}

write_spawn_config() {
  local path="$1"
  local workspace="$2"
  cat >"$path" <<JSON
{
  "workspace": "$workspace",
  "image_ref": "localhost/wrix-test:latest",
  "image_source": "",
  "env": [],
  "agent_args": ["echo", "hello"],
  "mounts": []
}
JSON
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

line_value() {
  local content="$1"
  local key="$2"
  printf '%s\n' "$content" | awk -F= -v key="$key" '$1 == key {print substr($0, length(key) + 2); exit}'
}

services_json_path() {
  local -a roots
  roots=("$XDG_STATE_HOME"/wrix/workspaces/*/services.json)
  if [[ ! -f "${roots[0]}" ]]; then
    fail "services.json was not created under $XDG_STATE_HOME"
    return 1
  fi
  printf '%s\n' "${roots[0]}"
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label missing '$needle'"
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label unexpectedly contained '$needle'"
  fi
}

assert_file_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label missing '$needle' in $file"
  fi
}

assert_file_not_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label unexpectedly found '$needle' in $file"
  fi
}

assert_port_range() {
  local label="$1"
  local port="$2"
  if (( port < 21000 || port > 22999 )); then
    fail "$label port $port is outside 21000-22999"
  fi
}

sandbox_dry_run() {
  local subcommand="$1"
  local workspace="$2"
  local profile_config="$3"
  local wrix_bin="$4"
  local spawn_config="$TEST_TMP/spawn.json"
  if [[ "$subcommand" == "spawn" ]]; then
    write_spawn_config "$spawn_config" "$workspace"
    WRIX_DRY_RUN=1 WRIX_DRY_RUN_SERVICES=1 "$wrix_bin" --profile-config "$profile_config" spawn --spawn-config "$spawn_config" --stdio
  else
    WRIX_DRY_RUN=1 WRIX_DRY_RUN_SERVICES=1 "$wrix_bin" --profile-config "$profile_config" run "$workspace" echo hello
  fi
}

prepare_launcher_workspace() {
  local name="$1"
  local profile_config="$TEST_TMP/$name-profile.json"
  local workspace="$TEST_TMP/$name-workspace"
  mkdir -p "$workspace"
  write_profile_config "$profile_config"
  printf '%s\n%s\n' "$workspace" "$profile_config"
}

test_container_pull_config() {
  require_python
  local package wrix_bin workspace profile_config run_output spawn_output endpoints state_root public_key cache_port cache_url
  local -a prepared
  package="$(build_wrix_package)"
  wrix_bin="$package/bin/wrix"
  with_fake_service_env "pull-config"
  mapfile -t prepared < <(prepare_launcher_workspace "pull-config")
  workspace="${prepared[0]}"
  profile_config="${prepared[1]}"

  run_output="$(sandbox_dry_run run "$workspace" "$profile_config" "$wrix_bin")"
  spawn_output="$(sandbox_dry_run spawn "$workspace" "$profile_config" "$wrix_bin")"
  endpoints="$(services_json_path)"
  state_root="$(json_get "$endpoints" state_root)"
  public_key="$(tr -d '\n' <"$state_root/keys/cache.pub")"
  cache_port="$(json_get "$endpoints" endpoints.cache_http.port)"
  cache_url="http://127.0.0.1:$cache_port"

  assert_contains "run output" "$run_output" "PROJECT_CACHE_URL=$cache_url"
  assert_contains "run output" "$run_output" "extra-substituters = $cache_url"
  assert_contains "run output" "$run_output" "extra-trusted-public-keys = $public_key"
  assert_contains "run output" "$run_output" "builders-use-substitutes = true"
  assert_contains "run output" "$run_output" "ENV=WRIX_PROJECT_CACHE_HOST=127.0.0.1"
  assert_contains "run output" "$run_output" "ENV=WRIX_PROJECT_CACHE_PORT=$cache_port"

  assert_contains "spawn output" "$spawn_output" "SUBCOMMAND=spawn"
  assert_contains "spawn output" "$spawn_output" "PROJECT_CACHE_URL=$cache_url"
  assert_contains "spawn output" "$spawn_output" "extra-substituters = $cache_url"
  assert_contains "spawn output" "$spawn_output" "extra-trusted-public-keys = $public_key"
  assert_contains "spawn output" "$spawn_output" "builders-use-substitutes = true"
}

test_no_container_dns_dependency() {
  require_python
  local package wrix_bin workspace profile_config first_output second_output endpoints cache_port first_url second_url run_file service_args
  local -a prepared
  package="$(build_wrix_package)"
  wrix_bin="$package/bin/wrix"
  with_fake_service_env "no-dns"
  mapfile -t prepared < <(prepare_launcher_workspace "no-dns")
  workspace="${prepared[0]}"
  profile_config="${prepared[1]}"

  first_output="$(sandbox_dry_run run "$workspace" "$profile_config" "$wrix_bin")"
  second_output="$(sandbox_dry_run run "$workspace" "$profile_config" "$wrix_bin")"
  endpoints="$(services_json_path)"
  cache_port="$(json_get "$endpoints" endpoints.cache_http.port)"
  first_url="$(line_value "$first_output" PROJECT_CACHE_URL)"
  second_url="$(line_value "$second_output" PROJECT_CACHE_URL)"
  run_file="$WRIX_FAKE_RUNTIME_STATE/run-no-dns-workspace-service"
  service_args="$(cat "$run_file")"

  assert_port_range "cache endpoint" "$cache_port"
  [[ "$first_url" =~ ^http://[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:[0-9]+$ ]] || fail "cache URL is not numeric: $first_url"
  assert_contains "stable cache URL" "$second_url" "$first_url"
  assert_not_contains "cache URL" "$first_url" "no-dns-workspace-service"
  assert_contains "service port publish" "$service_args" "127.0.0.1:$cache_port:8080"
  assert_contains "service command" "$service_args" "wrix-cache-serve /cache"
}

test_no_host_store_or_cache_secret() {
  require_python
  local package wrix_bin workspace profile_config output endpoints state_root cache_root secret_leaf store_mount
  local -a prepared
  package="$(build_wrix_package)"
  wrix_bin="$package/bin/wrix"
  with_fake_service_env "no-secret"
  mapfile -t prepared < <(prepare_launcher_workspace "no-secret")
  workspace="${prepared[0]}"
  profile_config="${prepared[1]}"

  output="$(sandbox_dry_run spawn "$workspace" "$profile_config" "$wrix_bin")"
  endpoints="$(services_json_path)"
  state_root="$(json_get "$endpoints" state_root)"
  cache_root="$(json_get "$endpoints" cache_root)"
  secret_leaf="cache.secret"
  store_mount=":/nix/store"

  assert_contains "sandbox output" "$output" "ENV=NIX_CONFIG="
  assert_not_contains "sandbox output" "$output" "$secret_leaf"
  assert_not_contains "sandbox output" "$output" "$state_root"
  assert_not_contains "sandbox output" "$output" "$cache_root"
  assert_not_contains "sandbox output" "$output" "$store_mount"
  assert_not_contains "sandbox config" "$output" "file://"
}

extract_policy_block() {
  local source="$1"
  local out="$2"
  awk '
    /^# BEGIN wrix network policy$/ { capture = 1 }
    capture { print }
    /^# END wrix network policy$/ { exit }
  ' "$source" >"$out"
}

write_firewall_stub() {
  local path="$1"
  local family="$2"
  cat >"$path" <<STUB
#!$BASH_BIN
set -euo pipefail
printf '%s %s\n' '$family' "\$*" >>"\${WRIX_STUB_FIREWALL_LOG:?}"
args=" \$* "
if [[ "\$args" = *" -S INPUT "* ]]; then
  printf '%s\n' '-P INPUT DROP'
  exit 0
fi
if [[ "\$args" = *" -S OUTPUT "* ]]; then
  printf '%s\n' '-P OUTPUT DROP'
  exit 0
fi
if [[ "\$args" = *" -C OUTPUT -d 10.0.0.0/8 -j REJECT "* ]]; then
  exit 0
fi
exit 0
STUB
  chmod +x "$path"
}

write_getent_stub() {
  local path="$1"
  cat >"$path" <<STUB
#!$BASH_BIN
set -euo pipefail
printf 'getent %s\n' "\$*" >>"\${WRIX_STUB_GETENT_LOG:?}"
exit 2
STUB
  chmod +x "$path"
}

prepare_policy_stubs() {
  local dir="$1"
  mkdir -p "$dir"
  write_firewall_stub "$dir/iptables" iptables
  write_firewall_stub "$dir/ip6tables" ip6tables
  write_getent_stub "$dir/getent"
  : >"$dir/getent.log"
}

run_policy() {
  local platform="$1"
  local stub_dir="$2"
  local block="$stub_dir/$platform-policy.sh"
  case "$platform" in
    linux) extract_policy_block "$LINUX_ENTRYPOINT" "$block" ;;
    darwin) extract_policy_block "$DARWIN_ENTRYPOINT" "$block" ;;
    *) fail "unknown platform: $platform" ;;
  esac
  WRIX_IPTABLES_BIN="$stub_dir/iptables" \
  WRIX_IP6TABLES_BIN="$stub_dir/ip6tables" \
  WRIX_GETENT_BIN="$stub_dir/getent" \
  WRIX_AWK_BIN="$AWK_BIN" \
  WRIX_SORT_BIN="$SORT_BIN" \
  WRIX_GREP_BIN="$GREP_BIN" \
  WRIX_STUB_FIREWALL_LOG="$stub_dir/firewall.log" \
  WRIX_STUB_GETENT_LOG="$stub_dir/getent.log" \
  WRIX_NETWORK="limit" \
  WRIX_NETWORK_ALLOWLIST="" \
  WRIX_NETWORK_DNS_SERVERS="" \
  WRIX_NETWORK_LOCAL_ENDPOINTS="" \
  WRIX_PROJECT_CACHE_HOST="127.0.0.1" \
  WRIX_PROJECT_CACHE_PORT="21042" \
  "$BASH_BIN" -c ". '$block'; apply_wrix_network_policy" \
    >"$stub_dir/$platform.out" 2>"$stub_dir/$platform.err"
}

test_limit_mode_cache_endpoint() {
  local platform stub_dir log
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/limit-$platform"
    prepare_policy_stubs "$stub_dir"
    run_policy "$platform" "$stub_dir"
    log="$stub_dir/firewall.log"
    assert_file_contains "$platform policy" "$log" "iptables -w -A OUTPUT -p tcp -d 127.0.0.1 --dport 21042 -j ACCEPT"
    assert_file_contains "$platform policy" "$log" "iptables -w -A OUTPUT -d 127.0.0.0/8 -j REJECT"
    assert_file_not_contains "$platform policy" "$log" "--dport 21043 -j ACCEPT"
    assert_file_not_contains "$platform policy" "$log" "iptables -w -A OUTPUT -j ACCEPT"
    if [[ -s "$stub_dir/getent.log" ]]; then
      fail "$platform resolved the numeric project cache endpoint through name service"
    fi
  done
}

ALL_TESTS=(
  test_container_pull_config
  test_no_container_dns_dependency
  test_no_host_store_or_cache_secret
  test_limit_mode_cache_endpoint
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
