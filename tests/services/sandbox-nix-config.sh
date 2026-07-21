#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
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

expected_source_kind() {
  case "$(uname -s)" in
    Darwin) printf 'docker-archive\n' ;;
    *) printf 'nix-descriptor\n' ;;
  esac
}

expected_sandbox_cache_host() {
  case "$(uname -s)" in
    Darwin) printf '127.0.0.1\n' ;;
    *) printf '169.254.1.2\n' ;;
  esac
}

require_python() {
  if command -v python3 >/dev/null 2>&1 || command -v jq >/dev/null 2>&1; then
    return 0
  fi
  printf 'SKIP: python3 or jq is required for JSON assertions\n' >&2
  exit 77
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
  nix build \
    --impure --no-warn-dirty \
    --out-link "$out_link" \
    --expr "
      let
        flake = builtins.getFlake \"git+file://$REPO_ROOT\";
        system = builtins.currentSystem;
        lib = flake.legacyPackages.\${system}.lib;
      in (lib.mkSandbox { profile = lib.profiles.base; }).launcher
    " >/dev/null
  WRIX_PACKAGE="$out_link"
  printf '%s\n' "$WRIX_PACKAGE"
}

build_cache_serve() {
  cargo build --quiet -p wrix-cache --bin wrix-cache-serve
  printf '%s\n' "$REPO_ROOT/target/debug/wrix-cache-serve"
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

log_call() {
  printf '%s\n' "$*" >>"$STATE_DIR/calls"
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
    printf '%s\n' "$*" >"$(run_file "$name")"
    log_call "$*"
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
  unset WRIX_PROJECT_CACHE_SANDBOX_HOST WRIX_SERVICE_IMAGE WRIX_SERVICE_IMAGE_SOURCE
  mkdir -p "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
}

write_profile_config() {
  local path="$1"
  local source_kind
  source_kind=$(expected_source_kind)
  cat >"$path" <<JSON
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
    "source_kind": "$source_kind",
    "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
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
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$file" "$path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)
for part in sys.argv[2].split('.'):
    value = value[part]
print(value)
PY
  else
    jq -r --arg path "$path" 'getpath($path | split(".")) | if . == null then "None" else . end' "$file"
  fi
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

assert_cache_server_policy() {
  if ! command -v curl >/dev/null 2>&1 || ! command -v timeout >/dev/null 2>&1; then
    printf 'SKIP: curl and timeout are required for cache server assertions\n' >&2
    exit 77
  fi
  local serve_bin cache_root pid body status attempt
  serve_bin="$(build_cache_serve)"
  cache_root="$TEST_TMP/no-dns-cache-root"
  mkdir -p "$cache_root/nar" "$cache_root/log"
  printf 'StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n' >"$cache_root/nix-cache-info"
  printf 'StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo\nSig: fake\n' >"$cache_root/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"
  printf 'nar payload\n' >"$cache_root/nar/demo.nar"

  timeout 15s "$serve_bin" "$cache_root" >"$TEST_TMP/cache-serve.out" 2>"$TEST_TMP/cache-serve.err" &
  pid="$!"
  for ((attempt = 0; attempt < 50; attempt++)); do
    if curl -fsS http://127.0.0.1:8080/nix-cache-info >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done

  body="$(curl -fsS http://127.0.0.1:8080/nix-cache-info)" || fail "cache server did not serve nix-cache-info: $(cat "$TEST_TMP/cache-serve.err")"
  assert_contains "cache server nix-cache-info" "$body" "StoreDir: /nix/store"
  status="$(curl -sS -o /dev/null -w '%{http_code}' -I http://127.0.0.1:8080/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo)"
  [[ "$status" == "200" ]] || fail "cache server HEAD narinfo status was $status"
  status="$(curl -sS -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8080/nar/demo.nar)"
  [[ "$status" == "405" ]] || fail "cache server write method status was $status"
  status="$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/)"
  [[ "$status" == "404" ]] || fail "cache server directory listing status was $status"
  status="$(curl --path-as-is -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/nar/../nix-cache-info)"
  [[ "$status" == "404" ]] || fail "cache server traversal status was $status"
  kill "$pid" 2>/dev/null || true # best-effort: timeout may already have stopped the helper.
  wait "$pid" 2>/dev/null || true # best-effort: helper shutdown can race with wait.
}

sandbox_dry_run() {
  local subcommand="$1"
  local workspace="$2"
  local profile_config="$3"
  local wrix_bin="$4"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/sandbox-dry-run-deploy-key"
  if [[ "$subcommand" == "spawn" ]]; then
    write_spawn_config "$spawn_config" "$workspace"
    printf 'sandbox dry-run deploy key fixture\n' >"$deploy_key"
    env -u WRIX_SIGNING_KEY \
      WRIX_DEPLOY_KEY="$deploy_key" \
      WRIX_GIT_SIGN=0 \
      WRIX_DRY_RUN=1 \
      WRIX_DRY_RUN_SERVICES=1 \
      "$wrix_bin" --profile-config "$profile_config" spawn --spawn-config "$spawn_config" --stdio
  else
    WRIX_DRY_RUN=1 WRIX_DRY_RUN_SERVICES=1 "$wrix_bin" --profile-config "$profile_config" run "$workspace" echo hello
  fi
}

prepare_launcher_workspace() {
  local name="$1"
  local profile_config="$TEST_TMP/$name-profile.json"
  local workspace="$TEST_TMP/$name-workspace"
  mkdir -p "$workspace/.git"
  write_profile_config "$profile_config"
  printf '%s\n%s\n' "$workspace" "$profile_config"
}

test_container_pull_config() {
  require_python
  local package wrix_bin workspace profile_config run_output spawn_output endpoints state_root public_key cache_host cache_port cache_url
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
  cache_host="$(expected_sandbox_cache_host)"
  cache_port="$(json_get "$endpoints" endpoints.cache_http.port)"
  cache_url="http://$cache_host:$cache_port"

  assert_contains "run output" "$run_output" "PROJECT_CACHE_URL=$cache_url"
  assert_contains "run output" "$run_output" "extra-substituters = $cache_url"
  assert_contains "run output" "$run_output" "extra-trusted-public-keys = $public_key"
  assert_contains "run output" "$run_output" "builders-use-substitutes = true"
  assert_contains "run output" "$run_output" "ENV=WRIX_PROJECT_CACHE_HOST=$cache_host"
  assert_contains "run output" "$run_output" "ENV=WRIX_PROJECT_CACHE_PORT=$cache_port"

  assert_contains "spawn output" "$spawn_output" "SUBCOMMAND=spawn"
  assert_contains "spawn output" "$spawn_output" "PROJECT_CACHE_URL=$cache_url"
  assert_contains "spawn output" "$spawn_output" "extra-substituters = $cache_url"
  assert_contains "spawn output" "$spawn_output" "extra-trusted-public-keys = $public_key"
  assert_contains "spawn output" "$spawn_output" "builders-use-substitutes = true"
}

test_loom_bead_spawn_uses_repo_service() {
  require_python
  local package wrix_bin repo bead repo_real profile_config output
  package="$(build_wrix_package)"
  wrix_bin="$package/bin/wrix"
  with_fake_service_env "loom-bead-spawn"
  repo="$TEST_TMP/loom-bead-spawn-repo"
  bead="$repo/.loom/beads/lm-gzgw.3"
  profile_config="$TEST_TMP/loom-bead-spawn-profile.json"
  mkdir -p "$repo/.git" "$repo/.beads/dolt" "$bead/.git" "$bead/.beads"
  repo_real="$(cd "$repo" && pwd -P)"
  write_profile_config "$profile_config"

  output="$(sandbox_dry_run spawn "$bead" "$profile_config" "$wrix_bin")"

  if ! "$WRIX_CONTAINER_RUNTIME" container exists loom-bead-spawn-repo-service; then
    fail "loom bead spawn did not start the repository service"
  fi
  if "$WRIX_CONTAINER_RUNTIME" container exists lm-gzgw.3-service; then
    fail "loom bead spawn started a bead-clone service container"
  fi
  assert_contains "loom bead output" "$output" "WORKSPACE=$bead"
  assert_contains "loom bead output" "$output" "ENV=BEADS_DOLT_SERVER_SOCKET=/run/wrix/dolt/dolt.sock"
  assert_contains "loom bead output" "$output" "MOUNT=-v $repo_real/.wrix:/run/wrix/dolt:rw"
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
  assert_cache_server_policy
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

ALL_TESTS=(
  test_container_pull_config
  test_loom_bead_spawn_uses_repo_service
  test_no_container_dns_dependency
  test_no_host_store_or_cache_secret
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
