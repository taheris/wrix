#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-lifecycle.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_python() {
  if command -v python3 >/dev/null 2>&1 || command -v jq >/dev/null 2>&1; then
    return 0
  fi
  printf 'SKIP: python3 or jq is required for JSON assertions\n' >&2
  exit 77
}

require_nix() {
  if ! command -v nix >/dev/null 2>&1; then
    printf 'SKIP: nix is required for image label assertions\n' >&2
    exit 77
  fi
}

build_wrix() {
  if [[ -n "${WRIX_TEST_WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_TEST_WRIX_BIN"
    return 0
  fi
  cargo build --quiet -p wrix-cli --bin wrix || return 1
  printf '%s\n' "$REPO_ROOT/target/debug/wrix"
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

image_file() {
  local name="$1"
  name="${name//\//_}"
  name="${name//:/_}"
  printf '%s/%s.image\n' "$STATE_DIR" "$name"
}

image_exists() {
  local name="$1"
  [[ -f "$(image_file "$name")" ]]
}

write_image() {
  local name="$1"
  local detail="$2"
  printf '%s\n' "$detail" >"$(image_file "$name")"
}

log_call() {
  printf '%s\n' "$*" >>"$STATE_DIR/calls"
}

last_arg() {
  local value=""
  local arg
  for arg in "$@"; do
    value="$arg"
  done
  printf '%s\n' "$value"
}

input_arg() {
  local previous=""
  local arg
  for arg in "$@"; do
    if [[ "$previous" == "--input" ]]; then
      printf '%s\n' "$arg"
      return 0
    fi
    previous="$arg"
  done
  return 1
}

label_value() {
  local name="$1"
  local label="$2"
  local run_path
  run_path="$(run_file "$name")"
  if [[ ! -f "$run_path" ]]; then
    return 1
  fi
  local run_args
  run_args="$(<"$run_path")"
  local previous=""
  local token
  local prefix="$label="
  # intentional word-splitting: fake runtime stores argv as a space-delimited log line.
  for token in $run_args; do
    if [[ "$previous" == "--label" && "$token" == "$prefix"* ]]; then
      printf '%s\n' "${token#"$prefix"}"
      return 0
    fi
    previous="$token"
  done
  return 1
}

host_ports_for_args() {
  local run_args="$1"
  local previous=""
  local token
  local mapping
  local rest
  local host_port
  # intentional word-splitting: fake runtime stores argv as a space-delimited log line.
  for token in $run_args; do
    if [[ "$previous" == "-p" || "$previous" == "--publish" ]]; then
      mapping="$token"
      case "$mapping" in
        127.0.0.1:*:*)
          rest="${mapping#127.0.0.1:}"
          host_port="${rest%%:*}"
          printf '%s\n' "$host_port"
          ;;
        *:*)
          host_port="${mapping%%:*}"
          printf '%s\n' "$host_port"
          ;;
      esac
    fi
    previous="$token"
  done
}

port_lines_for_args() {
  local run_args="$1"
  local previous=""
  local token
  local mapping
  local rest
  local host_port
  local container_port
  # intentional word-splitting: fake runtime stores argv as a space-delimited log line.
  for token in $run_args; do
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

first_conflicting_port() {
  local candidate_name="$1"
  local run_args="$2"
  local port
  local existing_file
  local existing_name
  local existing_args
  local existing_port
  # intentional word-splitting: fake runtime emits one host port per line.
  for port in $(host_ports_for_args "$run_args"); do
    for existing_file in "$STATE_DIR"/run-*; do
      [[ -e "$existing_file" ]] || continue
      existing_name="${existing_file##*/run-}"
      if [[ "$existing_name" == "$candidate_name" || ! -f "$(state_file "$existing_name")" ]]; then
        continue
      fi
      existing_args="$(<"$existing_file")"
      # intentional word-splitting: fake runtime emits one host port per line.
      for existing_port in $(host_ports_for_args "$existing_args"); do
        if [[ "$existing_port" == "$port" ]]; then
          printf '%s\n' "$port"
          return 0
        fi
      done
    done
  done
  return 1
}

case "${1:-}" in
  container)
    if [[ "${2:-}" == "exists" ]]; then
      if [[ -f "$(state_file "${3:-}")" ]]; then
        exit 0
      fi
      exit 1
    fi
    ;;
  image)
    case "${2:-}" in
      exists|inspect)
        name="$(last_arg "$@")"
        if image_exists "$name"; then
          if [[ "$*" == *'{{.Id}}'* ]]; then
            printf 'sha256:fake-image-id\n'
          fi
          exit 0
        fi
        exit 1
        ;;
      load)
        source="$(input_arg "$@")"
        loaded_ref="untagged@sha256:0000000000000000000000000000000000000000000000000000000000000000"
        write_image "$loaded_ref" "loaded from $source"
        log_call "$*"
        printf 'Loaded: %s\n' "$loaded_ref"
        ;;
      tag)
        source="${3:-}"
        target="${4:-}"
        if image_exists "$source" || [[ "$source" == sha256:* ]]; then
          write_image "$target" "tagged from $source"
          log_call "$*"
          exit 0
        fi
        exit 1
        ;;
      delete)
        target="${3:-}"
        rm -f "$(image_file "$target")"
        log_call "$*"
        ;;
      list)
        printf 'NAME TAG DIGEST\n'
        for image in "$STATE_DIR"/*.image; do
          [[ -e "$image" ]] || continue
          name="${image##*/}"
          name="${name%.image}"
          printf '%s latest sha256:fake\n' "$name"
        done
        ;;
    esac
    ;;
  load)
    source="$(input_arg "$@")"
    write_image "localhost/wrix-service:latest" "loaded from $source"
    log_call "$*"
    ;;
  info)
    printf 'overlay@%s/graph+%s/runroot\n' "$STATE_DIR" "$STATE_DIR"
    ;;
  tag)
    source="${2:-}"
    target="${3:-}"
    if image_exists "$source" || [[ "$source" == sha256:* ]]; then
      write_image "$target" "tagged from $source"
      log_call "$*"
      exit 0
    fi
    exit 1
    ;;
  inspect)
    name="${@: -1}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      printf 'Error: no such object: "%s"\n' "$name" >&2
      exit 1
    fi
    if [[ "$*" == *'{{.State.Running}}'* ]]; then
      printf 'true\n'
    elif [[ "$*" == *'index .Config.Labels "'* ]]; then
      args="$*"
      marker='index .Config.Labels "'
      label="${args#*"$marker"}"
      label="${label%%\"*}"
      if ! label_value "$name" "$label"; then
        printf '<no value>\n'
      fi
    else
      printf '[{"status":"running"}]\n'
    fi
    ;;
  port)
    name="${2:-}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      printf 'Error: no such object: "%s"\n' "$name" >&2
      exit 1
    fi
    port_lines_for_args "$(<"$(run_file "$name")")"
    ;;
  ps)
    for run_file in "$STATE_DIR"/run-*; do
      [[ -e "$run_file" ]] || continue
      name="${run_file##*/run-}"
      printf '%s\n' "$name"
    done
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
    if conflict_port="$(first_conflicting_port "$name" "$*")"; then
      printf 'pasta failed with exit code 1:\nFailed to bind port %s (Address already in use) for option '\''-t 127.0.0.1/%s-%s:8080-8080'\''\n' \
        "$conflict_port" "$conflict_port" "$conflict_port" >&2
      exit 1
    fi
    printf 'running\n' >"$(state_file "$name")"
    printf '%s\n' "$*" >"$(run_file "$name")"
    log_call "$*"
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

write_fake_skopeo() {
  local skopeo="$1"
  cat >"$skopeo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

STATE_DIR="${WRIX_FAKE_RUNTIME_STATE:?}"
mkdir -p "$STATE_DIR"

image_file() {
  local name="$1"
  name="${name//\//_}"
  name="${name//:/_}"
  printf '%s/%s.image\n' "$STATE_DIR" "$name"
}

log_call() {
  printf '%s\n' "skopeo $*" >>"$STATE_DIR/calls"
}

if [[ "${1:-}" == "--insecure-policy" ]]; then
  shift
fi
if [[ "${1:-}" != "copy" ]]; then
  printf 'unsupported fake skopeo command: %s\n' "$*" >&2
  exit 2
fi
shift
if [[ "${1:-}" == "--quiet" ]]; then
  shift
fi
source_ref="${1:-}"
store_ref="${2:-}"
log_call "--insecure-policy copy --quiet $source_ref $store_ref"
case "$source_ref" in
  oci:*)
    source_path="${source_ref#oci:}"
    source_path="${source_path%:*}"
    if [[ ! -d "$source_path" ]]; then
      printf 'missing OCI layout: %s\n' "$source_path" >&2
      exit 2
    fi
    ;;
  docker-archive:*)
    source_path="${source_ref#docker-archive:}"
    if [[ ! -f "$source_path" ]]; then
      printf 'missing docker archive: %s\n' "$source_path" >&2
      exit 2
    fi
    ;;
esac
if [[ "$store_ref" == containers-storage:* ]]; then
  image_ref="${store_ref#containers-storage:}"
  if [[ "$image_ref" == \[*\]* ]]; then
    image_ref="${image_ref#*]}"
  fi
  printf 'copied from %s\n' "$source_ref" >"$(image_file "$image_ref")"
fi
EOF
  chmod +x "$skopeo"
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

assert_service_descriptor_contract() {
  local descriptor="$1"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$descriptor" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    descriptor = json.load(handle)
legacy_stream_key = "fallback_" + "stream"
if legacy_stream_key in descriptor:
    print("FAIL: service descriptor exposes legacy stream fallback metadata", file=sys.stderr)
    sys.exit(1)
if descriptor.get("source_kind") != "nix-descriptor":
    print("FAIL: service descriptor source_kind is not nix-descriptor", file=sys.stderr)
    sys.exit(1)
if not descriptor.get("oci_layout"):
    print("FAIL: service descriptor is missing oci_layout", file=sys.stderr)
    sys.exit(1)
if not str(descriptor.get("digest", "")).startswith("sha256:"):
    print("FAIL: service descriptor is missing digest", file=sys.stderr)
    sys.exit(1)
PY
  else
    jq -e 'has("fallback_stream") | not' "$descriptor" >/dev/null \
      || fail "service descriptor exposes legacy stream fallback metadata"
    jq -e '.source_kind == "nix-descriptor"' "$descriptor" >/dev/null \
      || fail "service descriptor source_kind is not nix-descriptor"
    jq -e '(.oci_layout // "") != ""' "$descriptor" >/dev/null \
      || fail "service descriptor is missing oci_layout"
    jq -e '(.digest // "") | startswith("sha256:")' "$descriptor" >/dev/null \
      || fail "service descriptor is missing digest"
  fi
}

assert_service_image_metadata_contract() {
  local metadata_json="$1"
  local expected_source_kind="$2"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$metadata_json" "$expected_source_kind" <<'PY'
import json
import sys
metadata = json.loads(sys.argv[1])
expected_source_kind = sys.argv[2]
labels = metadata["labels"]
expected = {
    "wrix.managed": "true",
    "wrix.image.kind": "service",
}
for key, value in expected.items():
    actual = labels.get(key)
    if actual != value:
        print(f"FAIL: service image label {key}={actual!r}, expected {value!r}", file=sys.stderr)
        sys.exit(1)
if metadata["source_kind"] != expected_source_kind:
    print(
        f"FAIL: service image source_kind={metadata['source_kind']!r}, expected {expected_source_kind!r}",
        file=sys.stderr,
    )
    sys.exit(1)
for field in ["source", "digest"]:
    if not metadata[field].startswith("/nix/store/"):
        print(f"FAIL: service image {field} is not a store path: {metadata[field]!r}", file=sys.stderr)
        sys.exit(1)
if expected_source_kind == "nix-descriptor" and not metadata["ref"].startswith("localhost/wrix-service:"):
    print(f"FAIL: Linux service image ref lacks localhost prefix: {metadata['ref']!r}", file=sys.stderr)
    sys.exit(1)
if expected_source_kind == "docker-archive" and not metadata["ref"].startswith("wrix-service:"):
    print(f"FAIL: Darwin service image ref has unexpected shape: {metadata['ref']!r}", file=sys.stderr)
    sys.exit(1)
PY
  else
    printf '%s\n' "$metadata_json" | jq -e --arg source_kind "$expected_source_kind" '
      .labels["wrix.managed"] == "true" and
      .labels["wrix.image.kind"] == "service" and
      .source_kind == $source_kind and
      (.source | startswith("/nix/store/")) and
      (.digest | startswith("/nix/store/")) and
      (
        ($source_kind == "nix-descriptor" and (.ref | startswith("localhost/wrix-service:"))) or
        ($source_kind == "docker-archive" and (.ref | startswith("wrix-service:")))
      )
    ' >/dev/null || fail "service image metadata does not match the service image contract"
  fi
}

assert_equals() {
  local label="$1"
  local expected="$2"
  local actual="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: expected '$expected', got '$actual'"
  fi
}

assert_not_equals() {
  local label="$1"
  local first="$2"
  local second="$3"
  if [[ "$first" == "$second" ]]; then
    fail "$label: both values were '$first'"
  fi
}

assert_sha256_hex() {
  local label="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[0-9a-f]{64}$ ]]; then
    fail "$label: expected lowercase sha256 hex, got '$value'"
  fi
}

assert_port_range() {
  local label="$1"
  local port="$2"
  local start="$3"
  local end="$4"
  if (( port < start || port > end )); then
    fail "$label: port $port outside $start-$end"
  fi
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle'"
  fi
}

assert_file_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label: missing '$needle' in $file"
  fi
}

write_ps_only_stale_run_record() {
  local name="$1"
  local port="$2"
  printf 'run -d --name %s --label wrix.kind=service -p 127.0.0.1:%s:8080 image sh -c sleep\n' \
    "$name" \
    "$port" \
    >"$WRIX_FAKE_RUNTIME_STATE/run-$name"
}

with_fake_runtime_env() {
  local runtime_name="${1:-podman}"
  local runtime_dir="$TEST_TMP/runtime-bin"
  local state_dir="$TEST_TMP/runtime-state-$runtime_name"
  rm -rf "$runtime_dir" "$state_dir"
  mkdir -p "$runtime_dir" "$state_dir"
  write_fake_runtime "$runtime_dir/$runtime_name"
  write_fake_skopeo "$runtime_dir/skopeo"
  export PATH="$runtime_dir:$PATH"
  export WRIX_CONTAINER_RUNTIME="$runtime_dir/$runtime_name"
  export WRIX_FAKE_RUNTIME_STATE="$state_dir"
  export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
  unset WRIX_SERVICE_IMAGE WRIX_SERVICE_IMAGE_SOURCE WRIX_SERVICE_IMAGE_SOURCE_KIND WRIX_SERVICE_IMAGE_DIGEST
}

test_fake_runtime_contract() {
  with_fake_runtime_env
  "$WRIX_CONTAINER_RUNTIME" container exists demo && fail "demo should not exist before run"
  "$WRIX_CONTAINER_RUNTIME" run -d --name demo image sh -c 'sleep infinity'
  "$WRIX_CONTAINER_RUNTIME" container exists demo
  assert_equals "running inspect" "true" "$($WRIX_CONTAINER_RUNTIME inspect --format '{{.State.Running}}' demo)"
  "$WRIX_CONTAINER_RUNTIME" rm -f demo
  if "$WRIX_CONTAINER_RUNTIME" container exists demo; then
    fail "demo should be removed"
  fi

  "$WRIX_CONTAINER_RUNTIME" run -d \
    --name labelled \
    --label wrix.kind=service \
    --label wrix.workspace.hash=abc123 \
    -p 127.0.0.1:21042:8080 \
    image sh -c 'sleep infinity'
  assert_equals \
    "label inspect" \
    "abc123" \
    "$($WRIX_CONTAINER_RUNTIME inspect --format '{{ index .Config.Labels "wrix.workspace.hash" }}' labelled)"
  assert_contains "port listing" "$($WRIX_CONTAINER_RUNTIME port labelled)" "8080/tcp -> 127.0.0.1:21042"
  if "$WRIX_CONTAINER_RUNTIME" run -d --name conflict -p 127.0.0.1:21042:8080 image sh -c 'sleep infinity' 2>"$TEST_TMP/fake-conflict.err"; then
    fail "fake runtime allowed duplicate host port"
  fi
  assert_file_contains "fake bind error" "$TEST_TMP/fake-conflict.err" "Failed to bind port 21042"
}

test_workspace_identity() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local first_ws="$TEST_TMP/repo-one"
  local second_ws="$TEST_TMP/repo-two"
  mkdir -p "$first_ws/.git" "$second_ws/.git"

  (cd "$first_ws" && "$wrix_bin" service start >"$TEST_TMP/first-start.txt")
  (cd "$first_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/first-endpoints.json")
  (cd "$first_ws" && "$wrix_bin" service start >"$TEST_TMP/first-start-again.txt")
  (cd "$first_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/first-endpoints-again.json")
  (cd "$second_ws" && "$wrix_bin" service start >"$TEST_TMP/second-start.txt")
  (cd "$second_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/second-endpoints.json")

  local first_name first_name_again second_name
  first_name="$(json_get "$TEST_TMP/first-endpoints.json" container_name)"
  first_name_again="$(json_get "$TEST_TMP/first-endpoints-again.json" container_name)"
  second_name="$(json_get "$TEST_TMP/second-endpoints.json" container_name)"
  assert_equals "first container name" "repo-one-service" "$first_name"
  assert_equals "stable container name" "$first_name" "$first_name_again"
  assert_equals "second container name" "repo-two-service" "$second_name"

  local first_hash first_hash_again second_hash
  first_hash="$(json_get "$TEST_TMP/first-endpoints.json" workspace_hash)"
  first_hash_again="$(json_get "$TEST_TMP/first-endpoints-again.json" workspace_hash)"
  second_hash="$(json_get "$TEST_TMP/second-endpoints.json" workspace_hash)"
  assert_sha256_hex "first workspace hash" "$first_hash"
  assert_sha256_hex "second workspace hash" "$second_hash"
  assert_equals "stable workspace hash" "$first_hash" "$first_hash_again"
  assert_not_equals "different checkout hash" "$first_hash" "$second_hash"

  local first_state first_state_again second_state first_cache second_cache first_port second_port
  first_state="$(json_get "$TEST_TMP/first-endpoints.json" state_root)"
  first_state_again="$(json_get "$TEST_TMP/first-endpoints-again.json" state_root)"
  second_state="$(json_get "$TEST_TMP/second-endpoints.json" state_root)"
  first_cache="$(json_get "$TEST_TMP/first-endpoints.json" cache_root)"
  second_cache="$(json_get "$TEST_TMP/second-endpoints.json" cache_root)"
  first_port="$(json_get "$TEST_TMP/first-endpoints.json" endpoints.cache_http.port)"
  second_port="$(json_get "$TEST_TMP/second-endpoints.json" endpoints.cache_http.port)"
  assert_equals "stable state root" "$first_state" "$first_state_again"
  assert_contains "state root uses workspace hash" "$first_state" "$first_hash"
  assert_contains "cache root uses workspace hash" "$first_cache" "$first_hash"
  assert_not_equals "different state roots" "$first_state" "$second_state"
  assert_not_equals "different cache roots" "$first_cache" "$second_cache"
  assert_not_equals "different cache ports" "$first_port" "$second_port"
  assert_port_range "first cache port" "$first_port" 21000 22999
  assert_port_range "second cache port" "$second_port" 21000 22999

  local same_parent_one="$TEST_TMP/same-one"
  local same_parent_two="$TEST_TMP/same-two"
  local same_first_ws="$same_parent_one/repo"
  local same_second_ws="$same_parent_two/repo"
  mkdir -p "$same_first_ws/.git" "$same_second_ws/.git"
  (cd "$same_first_ws" && "$wrix_bin" service start >"$TEST_TMP/same-first-start.txt")
  (cd "$same_first_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/same-first-endpoints.json")
  (cd "$same_second_ws" && "$wrix_bin" service start >"$TEST_TMP/same-second-start.txt")
  (cd "$same_second_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/same-second-endpoints.json")

  local same_first_name same_second_name same_first_hash same_second_hash
  same_first_name="$(json_get "$TEST_TMP/same-first-endpoints.json" container_name)"
  same_second_name="$(json_get "$TEST_TMP/same-second-endpoints.json" container_name)"
  same_first_hash="$(json_get "$TEST_TMP/same-first-endpoints.json" workspace_hash)"
  same_second_hash="$(json_get "$TEST_TMP/same-second-endpoints.json" workspace_hash)"
  assert_not_equals "same basename container names" "$same_first_name" "$same_second_name"
  assert_not_equals "same basename hashes" "$same_first_hash" "$same_second_hash"
  assert_contains "same basename first name" "$same_first_name" "repo"
  assert_contains "same basename second name" "$same_second_name" "repo"
  "$WRIX_CONTAINER_RUNTIME" container exists "$same_first_name"
  "$WRIX_CONTAINER_RUNTIME" container exists "$same_second_name"

  assert_file_contains "detached run" "$WRIX_FAKE_RUNTIME_STATE/run-repo-one-service" "run -d --name repo-one-service"
  assert_file_contains "workspace hash label" "$WRIX_FAKE_RUNTIME_STATE/run-repo-one-service" "wrix.workspace.hash=$first_hash"
}

test_start_replaces_stale_same_workspace_service_on_cache_port() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-stale-service"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-stale-service"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-stale-service"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/stale-service-repo"
  mkdir -p "$workspace/.git"
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/stale-service-endpoints-before.json")

  local workspace_hash cache_port planned_name
  workspace_hash="$(json_get "$TEST_TMP/stale-service-endpoints-before.json" workspace_hash)"
  cache_port="$(json_get "$TEST_TMP/stale-service-endpoints-before.json" endpoints.cache_http.port)"
  planned_name="$(json_get "$TEST_TMP/stale-service-endpoints-before.json" container_name)"

  "$WRIX_CONTAINER_RUNTIME" run -d \
    --name loom-service \
    --label wrix.kind=service \
    --label "wrix.workspace.hash=$workspace_hash" \
    -p "127.0.0.1:$cache_port:8080" \
    image sh -c 'sleep infinity'

  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/stale-service-start.txt")

  if "$WRIX_CONTAINER_RUNTIME" container exists loom-service; then
    fail "stale same-workspace service container was not removed"
  fi
  "$WRIX_CONTAINER_RUNTIME" container exists "$planned_name"
  assert_file_contains "stale service removed" "$WRIX_FAKE_RUNTIME_STATE/calls" "rm -f loom-service"
  assert_file_contains \
    "planned service kept selected cache port" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "-p 127.0.0.1:$cache_port:8080"
}

test_start_ignores_container_removed_after_ps() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-removed-after-ps"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-removed-after-ps"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-removed-after-ps"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/removed-after-ps-repo"
  mkdir -p "$workspace/.git"
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/removed-after-ps-endpoints.json")

  local cache_port planned_name
  cache_port="$(json_get "$TEST_TMP/removed-after-ps-endpoints.json" endpoints.cache_http.port)"
  planned_name="$(json_get "$TEST_TMP/removed-after-ps-endpoints.json" container_name)"

  write_ps_only_stale_run_record "admiring_albattani" "$cache_port"

  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/removed-after-ps-start.txt")

  "$WRIX_CONTAINER_RUNTIME" container exists "$planned_name"
  assert_file_contains \
    "planned service starts after stale ps entry" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "-p 127.0.0.1:$cache_port:8080"
}

test_cache_start_recreates_running_no_cache_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-cache-reconfigure"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-cache-reconfigure"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-cache-reconfigure"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/cache-reconfigure-repo"
  mkdir -p "$workspace/.git" "$workspace/.beads/dolt"
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/cache-reconfigure-no-cache-start.txt")
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/cache-reconfigure-cache-start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/cache-reconfigure-endpoints.json")

  local cache_port planned_name
  cache_port="$(json_get "$TEST_TMP/cache-reconfigure-endpoints.json" endpoints.cache_http.port)"
  planned_name="$(json_get "$TEST_TMP/cache-reconfigure-endpoints.json" container_name)"

  assert_file_contains "no-cache service removed" "$WRIX_FAKE_RUNTIME_STATE/calls" "rm -f $planned_name"
  assert_file_contains \
    "recreated service publishes cache" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "-p 127.0.0.1:$cache_port:8080"
  assert_file_contains \
    "recreated service labelled cache-enabled" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "--label wrix.cache.enabled=true"
}

test_dolt_start_recreates_running_cache_only_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-dolt-reconfigure"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-dolt-reconfigure"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-dolt-reconfigure"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/dolt-reconfigure-repo"
  mkdir -p "$workspace/.git"
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/dolt-reconfigure-endpoints.json")

  local planned_name
  planned_name="$(json_get "$TEST_TMP/dolt-reconfigure-endpoints.json" container_name)"

  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/dolt-reconfigure-cache-start.txt")
  mkdir -p "$workspace/.beads/dolt"
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/dolt-reconfigure-dolt-start.txt")

  assert_file_contains "cache-only service removed for dolt" "$WRIX_FAKE_RUNTIME_STATE/calls" "rm -f $planned_name"
  assert_file_contains \
    "recreated service disables cache for no-cache start" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "--label wrix.cache.enabled=false"
  assert_file_contains \
    "recreated service labelled dolt unix" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "--label wrix.dolt.transport=unix"
  assert_file_contains \
    "recreated service runs dolt" \
    "$WRIX_FAKE_RUNTIME_STATE/run-$planned_name" \
    "dolt sql-server"
}

test_start_recreates_running_service_with_missing_dolt_socket() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-missing-dolt-socket"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-missing-dolt-socket"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-missing-dolt-socket"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/missing-dolt-socket-repo"
  mkdir -p "$workspace/.git" "$workspace/.beads/dolt"
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/missing-dolt-socket-first.txt")
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/missing-dolt-socket-second.txt")

  local planned_name run_count
  planned_name="missing-dolt-socket-repo-service"
  run_count="$(grep -cF -- "run -d --name $planned_name" "$WRIX_FAKE_RUNTIME_STATE/calls")"

  assert_equals "missing Dolt socket service run count" "2" "$run_count"
  assert_file_contains \
    "missing Dolt socket service removed" \
    "$WRIX_FAKE_RUNTIME_STATE/calls" \
    "rm -f $planned_name"
}

test_start_reports_unrelated_cache_port_owner() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-unrelated-port"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-unrelated-port"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-unrelated-port"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/unrelated-port-repo"
  mkdir -p "$workspace/.git"
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/unrelated-port-endpoints.json")

  local cache_port planned_name
  cache_port="$(json_get "$TEST_TMP/unrelated-port-endpoints.json" endpoints.cache_http.port)"
  planned_name="$(json_get "$TEST_TMP/unrelated-port-endpoints.json" container_name)"

  "$WRIX_CONTAINER_RUNTIME" run -d \
    --name foreign-service \
    --label wrix.kind=service \
    --label "wrix.workspace.hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
    -p "127.0.0.1:$cache_port:8080" \
    image sh -c 'sleep infinity'

  if (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/unrelated-port-start.txt" 2>"$TEST_TMP/unrelated-port-error.txt"); then
    fail "service start succeeded with unrelated cache port owner"
  fi

  local error_text
  error_text="$(<"$TEST_TMP/unrelated-port-error.txt")"
  assert_contains \
    "unrelated port diagnostic" \
    "$error_text" \
    "service port $cache_port is already in use by container foreign-service"
  assert_contains "unrelated port action" "$error_text" "free the port before retrying"
  if [[ "$error_text" == *"pasta failed"* ]]; then
    fail "service start surfaced raw pasta bind text: $error_text"
  fi
  "$WRIX_CONTAINER_RUNTIME" container exists foreign-service
  if "$WRIX_CONTAINER_RUNTIME" container exists "$planned_name"; then
    fail "planned service started despite unrelated cache port owner"
  fi
}

test_temp_cache_only_workspace_does_not_start_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env
  unset WRIX_SERVICE_ALLOW_TEMP_CACHE

  export HOME="$TEST_TMP/home-temp-cache-only"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-temp-cache-only"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-temp-cache-only"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/temp-cache-only-workspace"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/temp-cache-only-start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/temp-cache-only-endpoints.json")

  if "$WRIX_CONTAINER_RUNTIME" container exists temp-cache-only-workspace-service; then
    fail "cache-only temp workspace started a persistent service container"
  fi
  local cache_endpoint
  cache_endpoint="$(json_get "$TEST_TMP/temp-cache-only-endpoints.json" endpoints.cache_http)"
  assert_equals "temp cache endpoint" "None" "$cache_endpoint"
}

test_loom_bead_workspace_uses_repo_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-loom-bead"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-loom-bead"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-loom-bead"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local repo="$TEST_TMP/loom-repo"
  local bead="$repo/.loom/beads/lm-gzgw.3"
  local repo_real
  mkdir -p "$repo/.git" "$bead/.git"
  repo_real="$(cd "$repo" && pwd -P)"
  (cd "$bead" && "$wrix_bin" service start >"$TEST_TMP/loom-bead-start.txt")
  (cd "$bead" && "$wrix_bin" service endpoints >"$TEST_TMP/loom-bead-endpoints.json")
  (cd "$repo" && "$wrix_bin" service endpoints >"$TEST_TMP/loom-repo-endpoints.json")

  local bead_hash repo_hash bead_port repo_port
  bead_hash="$(json_get "$TEST_TMP/loom-bead-endpoints.json" workspace_hash)"
  repo_hash="$(json_get "$TEST_TMP/loom-repo-endpoints.json" workspace_hash)"
  bead_port="$(json_get "$TEST_TMP/loom-bead-endpoints.json" endpoints.cache_http.port)"
  repo_port="$(json_get "$TEST_TMP/loom-repo-endpoints.json" endpoints.cache_http.port)"
  assert_sha256_hex "loom bead workspace hash" "$bead_hash"
  assert_equals "loom bead outer hash" "$repo_hash" "$bead_hash"
  assert_equals "loom bead outer cache port" "$repo_port" "$bead_port"

  if ! "$WRIX_CONTAINER_RUNTIME" container exists loom-repo-service; then
    fail "loom bead workspace did not start the repository service"
  fi
  if "$WRIX_CONTAINER_RUNTIME" container exists lm-gzgw.3-service; then
    fail "loom bead workspace started a bead-clone service container"
  fi
  assert_equals \
    "loom bead container name" \
    "loom-repo-service" \
    "$(json_get "$TEST_TMP/loom-bead-endpoints.json" container_name)"
  assert_equals \
    "loom bead workspace path" \
    "$repo_real" \
    "$(json_get "$TEST_TMP/loom-bead-endpoints.json" workspace_path)"
}

test_service_start_loads_image_source() {
  require_python
  require_nix
  local wrix_bin metadata_json metadata_file real_image real_source real_source_kind real_source_link
  wrix_bin="$(build_wrix)"
  metadata_json="$(nix eval --no-warn-dirty --json "$REPO_ROOT#wrix-service-image" --apply 'image: { inherit (image) ref source_kind; source = toString image.source; }')"
  metadata_file="$TEST_TMP/service-image-metadata.json"
  printf '%s\n' "$metadata_json" >"$metadata_file"
  real_image="$(json_get "$metadata_file" ref)"
  real_source="$(json_get "$metadata_file" source)"
  real_source_kind="$(json_get "$metadata_file" source_kind)"
  real_source_link="$TEST_TMP/wrix-service-image-source"
  nix build --no-warn-dirty --out-link "$real_source_link" "$REPO_ROOT#wrix-service-image.source" >/dev/null
  real_source="$real_source_link"

  if [[ "$real_source_kind" == "nix-descriptor" ]]; then
    with_fake_runtime_env

    export HOME="$TEST_TMP/home-image-source"
    export XDG_STATE_HOME="$TEST_TMP/xdg-state-image-source"
    export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-image-source"
    export WRIX_SERVICE_IMAGE="$real_image"
    export WRIX_SERVICE_IMAGE_SOURCE="$real_source"
    export WRIX_SERVICE_IMAGE_SOURCE_KIND="$real_source_kind"
    mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

    assert_service_descriptor_contract "$WRIX_SERVICE_IMAGE_SOURCE"
    local oci_layout oci_ref
    oci_layout="$(json_get "$WRIX_SERVICE_IMAGE_SOURCE" oci_layout)"
    oci_ref="$(json_get "$WRIX_SERVICE_IMAGE_SOURCE" oci_ref)"

    local workspace="$TEST_TMP/image-source-repo"
    mkdir -p "$workspace/.git"
    (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/image-source-start.txt")

    assert_file_contains \
      "service image descriptor OCI install" \
      "$WRIX_FAKE_RUNTIME_STATE/calls" \
      "skopeo --insecure-policy copy --quiet oci:$oci_layout:$oci_ref containers-storage:"
    if grep -F -- "load --input $WRIX_SERVICE_IMAGE_SOURCE" "$WRIX_FAKE_RUNTIME_STATE/calls" >/dev/null; then
      fail "nix-descriptor service image used tar load path"
    fi
    assert_file_contains \
      "service run after descriptor install" \
      "$WRIX_FAKE_RUNTIME_STATE/run-image-source-repo-service" \
      "$WRIX_SERVICE_IMAGE"
  fi

  with_fake_runtime_env container
  export HOME="$TEST_TMP/home-image-source-darwin"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-image-source-darwin"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-image-source-darwin"
  export WRIX_SERVICE_IMAGE="wrix-service:darwin-test"
  export WRIX_SERVICE_IMAGE_SOURCE="$TEST_TMP/wrix-service-archive.tar"
  export WRIX_SERVICE_IMAGE_SOURCE_KIND="docker-archive"
  if [[ "$real_source_kind" == "docker-archive" ]]; then
    export WRIX_SERVICE_IMAGE="$real_image"
    export WRIX_SERVICE_IMAGE_SOURCE="$real_source"
    export WRIX_SERVICE_IMAGE_SOURCE_KIND="$real_source_kind"
  fi
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  if [[ "$real_source_kind" != "docker-archive" ]]; then
    printf 'image archive\n' >"$WRIX_SERVICE_IMAGE_SOURCE"
  fi

  local darwin_workspace="$TEST_TMP/image-source-darwin-repo"
  mkdir -p "$darwin_workspace/.git"
  (cd "$darwin_workspace" && "$wrix_bin" service start >"$TEST_TMP/image-source-darwin-start.txt")

  assert_file_contains \
    "service image docker archive converted for container" \
    "$WRIX_FAKE_RUNTIME_STATE/calls" \
    "skopeo --insecure-policy copy --quiet docker-archive:$WRIX_SERVICE_IMAGE_SOURCE oci-archive:"
  assert_file_contains \
    "service image docker archive load" \
    "$WRIX_FAKE_RUNTIME_STATE/calls" \
    "image load --input"
  assert_file_contains \
    "service image docker archive tag" \
    "$WRIX_FAKE_RUNTIME_STATE/calls" \
    "image tag untagged@sha256:0000000000000000000000000000000000000000000000000000000000000000 $WRIX_SERVICE_IMAGE"
  assert_file_contains \
    "service image temporary load ref cleanup" \
    "$WRIX_FAKE_RUNTIME_STATE/calls" \
    "image delete untagged@sha256:0000000000000000000000000000000000000000000000000000000000000000"
  assert_file_contains \
    "service run after archive load" \
    "$WRIX_FAKE_RUNTIME_STATE/run-image-source-darwin-repo-service" \
    "$WRIX_SERVICE_IMAGE"
  if grep -F -- "--restart" "$WRIX_FAKE_RUNTIME_STATE/run-image-source-darwin-repo-service" >/dev/null; then
    fail "container runtime service run included unsupported --restart flag"
  fi
  local run_count
  run_count=$(grep -cF -- "run -d --name image-source-darwin-repo-service" "$WRIX_FAKE_RUNTIME_STATE/calls")
  (cd "$darwin_workspace" && "$wrix_bin" service start >"$TEST_TMP/image-source-darwin-start-again.txt")
  assert_equals \
    "container runtime second start run count" \
    "$run_count" \
    "$(grep -cF -- "run -d --name image-source-darwin-repo-service" "$WRIX_FAKE_RUNTIME_STATE/calls")"
}

test_service_image_labels() {
  require_python
  require_nix

  local expected_source_kind
  if [[ "$(uname -s)" == "Darwin" ]]; then
    expected_source_kind="docker-archive"
  else
    expected_source_kind="nix-descriptor"
  fi

  local metadata_json
  metadata_json="$(nix eval --no-warn-dirty --json "$REPO_ROOT#wrix-service-image" --apply 'image: { inherit (image) labels ref source_kind; source = toString image.source; digest = toString image.digest; }')"
  assert_service_image_metadata_contract "$metadata_json" "$expected_source_kind"
}

test_service_mounts_beads_worktree_remote() {
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-beads-remote"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-beads-remote"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-beads-remote"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/beads-remote-repo"
  local worktree_remote="$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  mkdir -p "$workspace/.beads/dolt" "$worktree_remote"
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/beads-remote-start.txt")

  assert_file_contains \
    "beads remote bind mount" \
    "$WRIX_FAKE_RUNTIME_STATE/run-beads-remote-repo-service" \
    "$worktree_remote:$worktree_remote:rw"
}

ALL_TESTS=(
  test_fake_runtime_contract
  test_workspace_identity
  test_start_replaces_stale_same_workspace_service_on_cache_port
  test_start_ignores_container_removed_after_ps
  test_cache_start_recreates_running_no_cache_service
  test_dolt_start_recreates_running_cache_only_service
  test_start_recreates_running_service_with_missing_dolt_socket
  test_start_reports_unrelated_cache_port_owner
  test_temp_cache_only_workspace_does_not_start_service
  test_loom_bead_workspace_uses_repo_service
  test_service_start_loads_image_source
  test_service_image_labels
  test_service_mounts_beads_worktree_remote
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
