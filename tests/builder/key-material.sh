#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-builder-keys.XXXXXX)"
WRIX_BUILDER_TEST_BIN=""

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  echo "FAIL: $message" >&2
  exit 1
}

require_command() {
  local command_name="$1"

  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "SKIP: $command_name is required" >&2
    exit 77
  fi
}

build_wrix_builder() {
  local output_path

  if [[ -n "${WRIX_BUILDER_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BUILDER_BIN"
    return 0
  fi

  if [[ -z "$WRIX_BUILDER_TEST_BIN" ]]; then
    require_command nix
    output_path="$(nix build --no-link --print-out-paths "$REPO_ROOT#wrix-builder")"
    WRIX_BUILDER_TEST_BIN="$output_path/bin/wrix-builder"
  fi

  printf '%s\n' "$WRIX_BUILDER_TEST_BIN"
}

write_fake_tools() {
  local bin_dir="$1"
  local bash_bin

  bash_bin="$(command -v bash)"
  mkdir -p "$bin_dir"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/container"
  cat >>"$bin_dir/container" <<'CONTAINER'
set -euo pipefail

log_file="${WRIX_BUILDER_FAKE_LOG:?}"
state_dir="${WRIX_BUILDER_FAKE_STATE:?}"
tar_root="${WRIX_BUILDER_FAKE_TAR_ROOT:?}"
mkdir -p "$state_dir/containers" "$state_dir/images"
printf 'container|%s\n' "$*" >>"$log_file"

image_file() {
  local ref="$1"
  local key="$ref"

  key="${key//\//_}"
  key="${key//:/_}"
  key="${key//@/_}"
  printf '%s\n' "$state_dir/images/$key"
}

image_write() {
  local ref="$1"
  local digest="$2"
  local managed="$3"
  local kind="$4"
  local path

  path="$(image_file "$ref")"
  printf '%s\n%s\n%s\n%s\n' "$ref" "$digest" "$managed" "$kind" >"$path"
}

image_read_field() {
  local ref="$1"
  local field="$2"
  local path

  path="$(image_file "$ref")"
  [[ -f "$path" ]] || return 1
  sed -n "${field}p" "$path"
}

image_remove() {
  local ref="$1"
  local path

  path="$(image_file "$ref")"
  rm -f "$path"
}

image_list() {
  local digest
  local first=true
  local kind
  local managed
  local path
  local ref

  printf '['
  for path in "$state_dir/images"/*; do
    [[ -f "$path" ]] || continue
    ref="$(sed -n '1p' "$path")"
    digest="$(sed -n '2p' "$path")"
    managed="$(sed -n '3p' "$path")"
    kind="$(sed -n '4p' "$path")"
    if [[ "$first" == true ]]; then
      first=false
    else
      printf ','
    fi
    printf '{"configuration":{"name":"%s"},"id":"%s","variants":[{"config":{"config":{"Labels":{"wrix.managed":"%s","wrix.image.kind":"%s"}}}}]}' \
      "$ref" \
      "$digest" \
      "$managed" \
      "$kind"
  done
  printf ']\n'
}

image_inspect() {
  local digest
  local kind
  local managed
  local ref="$1"

  digest="$(image_read_field "$ref" 2)" || return 1
  managed="$(image_read_field "$ref" 3)" || return 1
  kind="$(image_read_field "$ref" 4)" || return 1
  printf '[{"digest":"%s","variants":[{"config":{"config":{"Labels":{"wrix.managed":"%s","wrix.image.kind":"%s"}}}}]}]\n' \
    "$digest" \
    "$managed" \
    "$kind"
}

image_tag() {
  local digest
  local kind
  local managed
  local source="$1"
  local target="$2"

  digest="$(image_read_field "$source" 2)" || digest="sha256:$(printf '%064d' 1)"
  managed="$(image_read_field "$source" 3)" || managed="true"
  kind="$(image_read_field "$source" 4)" || kind="builder"
  image_write "$target" "$digest" "$managed" "$kind"
}

image_load() {
  local loaded_sha
  local loaded_ref

  loaded_sha="$(printf '%064d' 1)"
  loaded_ref="untagged@sha256:$loaded_sha"
  image_write "$loaded_ref" "sha256:$loaded_sha" "true" "builder"
  printf 'Loaded image: %s\n' "$loaded_ref"
}

container_file() {
  local name="$1"

  printf '%s\n' "$state_dir/containers/$name"
}

container_status() {
  local name="$1"
  local path

  path="$(container_file "$name")"
  if [[ ! -f "$path" ]]; then
    return 1
  fi
  cat "$path"
}

write_status() {
  local name="$1"
  local status="$2"
  local path

  path="$(container_file "$name")"
  printf '%s\n' "$status" >"$path"
}

remove_status() {
  local name="$1"
  local path

  path="$(container_file "$name")"
  rm -f "$path"
}

extract_name() {
  local expect_name=0
  local arg

  for arg in "$@"; do
    if [[ "$expect_name" == "1" ]]; then
      printf '%s\n' "$arg"
      return 0
    fi
    case "$arg" in
      --name=*)
        printf '%s\n' "${arg#--name=}"
        return 0
        ;;
      --name)
        expect_name=1
        ;;
    esac
  done
  return 1
}

case "${1:-}" in
  system)
    case "${2:-}" in
      status | start)
        exit 0
        ;;
    esac
    ;;
  network)
    if [[ "${2:-}" == "inspect" && "${3:-}" == "default" ]]; then
      cat <<'JSON'
[
  {
    "configuration" : {
      "name" : "default"
    },
    "status" : {
      "ipv4Gateway" : "192.168.64.1",
      "ipv4Subnet" : "192.168.64.0/24"
    }
  }
]
JSON
      exit 0
    fi
    ;;
  image)
    case "${2:-}" in
      inspect)
        if [[ "$#" -lt 3 ]]; then
          printf 'fake container: image inspect requires a ref\n' >&2
          exit 64
        fi
        image_inspect "$3"
        exit 0
        ;;
      delete)
        if [[ "$#" -lt 3 ]]; then
          printf 'fake container: image delete requires a ref\n' >&2
          exit 64
        fi
        image_remove "$3"
        exit 0
        ;;
      tag)
        if [[ "$#" -lt 4 ]]; then
          printf 'fake container: image tag requires source and target refs\n' >&2
          exit 64
        fi
        image_tag "$3" "$4"
        exit 0
        ;;
      prune)
        exit 0
        ;;
      list)
        if [[ "${3:-}" != "--format" || "${4:-}" != "json" ]]; then
          printf 'fake container: image list requires --format json\n' >&2
          exit 64
        fi
        image_list
        exit 0
        ;;
      load)
        image_load
        exit 0
        ;;
    esac
    ;;
  inspect)
    if [[ "$#" -lt 2 ]]; then
      printf 'fake container: inspect requires a name\n' >&2
      exit 64
    fi
    if status="$(container_status "$2")"; then
      cat <<JSON
[
  {
    "status" : {
      "state" : "$status"
    }
  }
]
JSON
    else
      printf '[]\n'
    fi
    exit 0
    ;;
  run)
    shift
    if ! name="$(extract_name "$@")"; then
      printf 'fake container: run requires --name\n' >&2
      exit 64
    fi
    write_status "$name" "running"
    printf '%s\n' "$name"
    exit 0
    ;;
  exec)
    if [[ "$#" -lt 3 ]]; then
      printf 'fake container: exec requires a name and command\n' >&2
      exit 64
    fi
    case "$3" in
      pgrep)
        exit 0
        ;;
      tar)
        tar -cf - -C "$tar_root" nix
        exit 0
        ;;
    esac
    ;;
  stop)
    if [[ "$#" -lt 2 ]]; then
      printf 'fake container: stop requires a name\n' >&2
      exit 64
    fi
    write_status "$2" "stopped"
    exit 0
    ;;
  rm)
    if [[ "$#" -lt 2 ]]; then
      printf 'fake container: rm requires a name\n' >&2
      exit 64
    fi
    remove_status "$2"
    exit 0
    ;;
esac

printf 'fake container: unexpected args: %s\n' "$*" >&2
exit 64
CONTAINER
  chmod +x "$bin_dir/container"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/sw_vers"
  cat >>"$bin_dir/sw_vers" <<'SWVERS'
set -euo pipefail

if [[ "${1:-}" == "-productVersion" ]]; then
  printf '26.0\n'
  exit 0
fi

printf 'fake sw_vers: unexpected args: %s\n' "$*" >&2
exit 64
SWVERS
  chmod +x "$bin_dir/sw_vers"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/sleep"
  cat >>"$bin_dir/sleep" <<'SLEEP'
set -euo pipefail

exit 0
SLEEP
  chmod +x "$bin_dir/sleep"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/route"
  cat >>"$bin_dir/route" <<'ROUTE'
set -euo pipefail

log_file="${WRIX_BUILDER_FAKE_LOG:?}"
if [[ "$*" == "-n get default" ]]; then
  cat <<'OUTPUT'
   route to: default
destination: default
  interface: utun11
OUTPUT
  exit 0
fi
printf 'route|%s\n' "$*" >>"$log_file"
ROUTE
  chmod +x "$bin_dir/route"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/netstat"
  cat >>"$bin_dir/netstat" <<'NETSTAT'
set -euo pipefail

printf 'default link#29 UCSg utun11\n'
NETSTAT
  chmod +x "$bin_dir/netstat"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/ifconfig"
  cat >>"$bin_dir/ifconfig" <<'IFCONFIG'
set -euo pipefail

cat <<'OUTPUT'
bridge100: flags=8a63<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST>
        inet 192.168.64.1 netmask 0xffffff00 broadcast 192.168.64.255
OUTPUT
IFCONFIG
  chmod +x "$bin_dir/ifconfig"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/sudo"
  cat >>"$bin_dir/sudo" <<'SUDO'
set -euo pipefail

log_file="${WRIX_BUILDER_FAKE_LOG:?}"
printf 'sudo|%s\n' "$*" >>"$log_file"
exec "$@"
SUDO
  chmod +x "$bin_dir/sudo"

  printf '#!%s\n' "$bash_bin" >"$bin_dir/skopeo"
  cat >>"$bin_dir/skopeo" <<'SKOPEO'
set -euo pipefail

log_file="${WRIX_BUILDER_FAKE_LOG:?}"
printf 'skopeo|%s\n' "$*" >>"$log_file"

if [[ "${1:-}" == "--insecure-policy" && "${2:-}" == "copy" && "${3:-}" == "--quiet" ]]; then
  source_ref="${4:-}"
  dest_ref="${5:-}"
  case "$source_ref" in
    docker-archive:*|oci:*) ;;
    *)
      printf 'fake skopeo: unsupported source ref: %s\n' "$source_ref" >&2
      exit 64
      ;;
  esac
  case "$dest_ref" in
    oci-archive:*)
      archive_path="${dest_ref#oci-archive:}"
      mkdir -p "$(dirname "$archive_path")"
      printf 'fake oci archive from %s\n' "$source_ref" >"$archive_path"
      exit 0
      ;;
  esac
fi

printf 'fake skopeo: unexpected args: %s\n' "$*" >&2
exit 64
SKOPEO
  chmod +x "$bin_dir/skopeo"
}

prepare_builder_fixture() {
  local test_root="$1"

  mkdir -p "$test_root/home" "$test_root/state" "$test_root/tar-root/nix/store"
  printf '%s\n' "seed" >"$test_root/tar-root/nix/store/seed"
  : >"$test_root/container.log"
  write_fake_tools "$test_root/bin"
}

fake_image_file() {
  local ref="$2"
  local state_dir="$1"
  local key="$ref"

  key="${key//\//_}"
  key="${key//:/_}"
  key="${key//@/_}"
  printf '%s\n' "$state_dir/images/$key"
}

fake_write_image() {
  local digest="$3"
  local kind="$5"
  local managed="$4"
  local path
  local ref="$2"
  local state_dir="$1"

  mkdir -p "$state_dir/images"
  path="$(fake_image_file "$state_dir" "$ref")"
  printf '%s\n%s\n%s\n%s\n' "$ref" "$digest" "$managed" "$kind" >"$path"
}

run_fake_container() {
  local test_root="$1"

  shift
  WRIX_BUILDER_FAKE_STATE="$test_root/state" \
    WRIX_BUILDER_FAKE_LOG="$test_root/container.log" \
    WRIX_BUILDER_FAKE_TAR_ROOT="$test_root/tar-root" \
    "$test_root/bin/container" "$@"
}

run_builder() {
  local test_root="$1"
  local builder
  local keygen_bin
  local base64_bin

  shift
  builder="$(build_wrix_builder)"
  keygen_bin="$(command -v ssh-keygen)"
  base64_bin="$(command -v base64)"

  PATH="$test_root/bin:$PATH" \
    HOME="$test_root/home" \
    XDG_DATA_HOME="" \
    XDG_CACHE_HOME="" \
    SUDO_USER="" \
    WRIX_BUILDER_FAKE_STATE="$test_root/state" \
    WRIX_BUILDER_FAKE_LOG="$test_root/container.log" \
    WRIX_BUILDER_FAKE_TAR_ROOT="$test_root/tar-root" \
    WRIX_BUILDER_SSH_KEYGEN="$keygen_bin" \
    WRIX_BUILDER_BASE64="$base64_bin" \
    WRIX_BUILDER_SKOPEO="$test_root/bin/skopeo" \
    "$builder" "$@"
}

mode_of() {
  local path="$1"
  if stat -c '%a' "$path" >/dev/null 2>&1; then # best-effort: probe GNU stat mode format support.
    stat -c '%a' "$path"
  else
    stat -f '%Lp' "$path"
  fi
}

assert_mode() {
  local path="$1"
  local expected="$2"
  local actual

  actual="$(mode_of "$path")"
  [[ "$actual" == "$expected" ]] || fail "$path mode is $actual, expected $expected"
}

base64_decode() {
  local path="$1"

  if base64 --decode "$path" >/dev/null 2>&1; then # best-effort: probe GNU base64 flag support.
    base64 --decode "$path"
  elif base64 -d "$path" >/dev/null 2>&1; then # best-effort: probe short decode flag support.
    base64 -d "$path"
  else
    base64 -D "$path"
  fi
}

assert_ed25519_public_key() {
  local path="$1"
  local key_type

  read -r key_type _ <"$path"
  [[ "$key_type" == "ssh-ed25519" ]] || fail "$path is $key_type, expected ssh-ed25519"
}

assert_file_contains() {
  local path="$1"
  local expected="$2"
  local message="$3"

  grep -Fq -- "$expected" "$path" || fail "$message"
}

assert_file_lacks() {
  local path="$1"
  local unexpected="$2"
  local message="$3"

  if grep -Fq -- "$unexpected" "$path"; then
    fail "$message"
  fi
}

test_fake_container_inspect_matches_apple_shape() {
  local inspect_output
  local test_root="$TEST_TMP/inspect-contract"

  require_command jq
  prepare_builder_fixture "$test_root"

  run_fake_container "$test_root" run --name wrix-builder fake-image >/dev/null
  inspect_output="$(run_fake_container "$test_root" inspect wrix-builder)"

  if ! printf '%s\n' "$inspect_output" | jq -e '
    type == "array" and
    length == 1 and
    (.[0].status | type == "object") and
    .[0].status.state == "running"
  ' >/dev/null; then
    fail "fake container inspect output does not match the Apple status shape"
  fi
  [[ "$inspect_output" == *$'\n'* ]] \
    || fail "fake container inspect output is not pretty-printed like Apple container"
}

test_generates_per_user_ed25519_material() {
  local test_root="$TEST_TMP/generate"
  local keys_dir="$test_root/home/.local/share/wrix/builder-keys"
  local decoded="$test_root/host-key.pub"

  require_command ssh-keygen
  require_command base64
  require_command tar
  prepare_builder_fixture "$test_root"

  run_builder "$test_root" start

  [[ -f "$keys_dir/host_ed25519" ]] || fail "host private key was not generated"
  [[ -f "$keys_dir/host_ed25519.pub" ]] || fail "host public key was not generated"
  [[ -f "$keys_dir/client_ed25519" ]] || fail "client private key was not generated"
  [[ -f "$keys_dir/client_ed25519.pub" ]] || fail "client public key was not generated"
  [[ -f "$keys_dir/public_host_key_base64" ]] || fail "public host key metadata was not generated"
  [[ -f "$keys_dir/known_hosts" ]] || fail "client known_hosts was not generated"

  assert_mode "$keys_dir" "700"
  assert_mode "$keys_dir/host_ed25519" "600"
  assert_mode "$keys_dir/client_ed25519" "600"
  assert_mode "$keys_dir/host_ed25519.pub" "644"
  assert_mode "$keys_dir/client_ed25519.pub" "644"
  assert_mode "$keys_dir/public_host_key_base64" "644"
  assert_mode "$keys_dir/known_hosts" "600"

  assert_ed25519_public_key "$keys_dir/host_ed25519.pub"
  assert_ed25519_public_key "$keys_dir/client_ed25519.pub"

  base64_decode "$keys_dir/public_host_key_base64" >"$decoded"
  cmp "$keys_dir/host_ed25519.pub" "$decoded" >/dev/null \
    || fail "base64 host-key metadata does not decode to the host public key"

  grep -Fxq "[localhost]:2222 $(cat "$keys_dir/host_ed25519.pub")" "$keys_dir/known_hosts" \
    || fail "known_hosts does not pin the generated host key"
  assert_file_contains \
    "$test_root/container.log" \
    "-v $keys_dir:/run/keys:ro" \
    "wrix-builder start did not mount the per-user key directory"
}

test_loads_image_through_source_kind_contract() {
  local expected_source_prefix="docker-archive:"
  local loaded_ref
  local test_root="$TEST_TMP/source-kind"

  loaded_ref="untagged@sha256:$(printf '%064d' 1)"
  require_command ssh-keygen
  require_command base64
  require_command tar
  prepare_builder_fixture "$test_root"

  run_builder "$test_root" start

  if ! grep -Eq "^skopeo\\|--insecure-policy copy --quiet ${expected_source_prefix}.+ oci-archive:" "$test_root/container.log"; then
    fail "wrix-builder start did not route the bootstrap image through the $expected_source_prefix source_kind transport"
  fi
  assert_file_contains \
    "$test_root/container.log" \
    "container|image load --input" \
    "wrix-builder start did not load the converted OCI archive with Apple container"
  assert_file_contains \
    "$test_root/container.log" \
    "container|image tag untagged@sha256:" \
    "wrix-builder start did not tag the loaded builder image ref"
  assert_file_contains \
    "$test_root/container.log" \
    "container|image delete $loaded_ref" \
    "wrix-builder start did not delete the temporary loaded builder image ref"
}

test_builder_cleanup_is_wrix_scoped() {
  local stale_managed_digest
  local stale_unlabelled_digest
  local test_root="$TEST_TMP/scoped-cleanup"

  stale_managed_digest="sha256:$(printf '%064d' 7)"
  stale_unlabelled_digest="sha256:$(printf '%064d' 6)"
  require_command ssh-keygen
  require_command base64
  require_command tar
  prepare_builder_fixture "$test_root"
  fake_write_image "$test_root/state" "stale-labelled:old" "sha256:$(printf '%064d' 2)" "true" "builder"
  fake_write_image "$test_root/state" "wrix-builder:old" "sha256:$(printf '%064d' 3)" "" ""
  fake_write_image "$test_root/state" "wrix-profile:old" "sha256:$(printf '%064d' 4)" "true" "profile"
  fake_write_image "$test_root/state" "user-image:latest" "sha256:$(printf '%064d' 5)" "" ""
  fake_write_image "$test_root/state" "untagged@${stale_unlabelled_digest}" "$stale_unlabelled_digest" "" ""
  fake_write_image "$test_root/state" "untagged@${stale_managed_digest}" "$stale_managed_digest" "true" "builder"

  run_builder "$test_root" start

  assert_file_contains \
    "$test_root/container.log" \
    "container|image delete stale-labelled:old" \
    "builder cleanup did not delete the stale labelled builder image"
  assert_file_contains \
    "$test_root/container.log" \
    "container|image delete wrix-builder:old" \
    "builder cleanup did not delete the legacy wrix-builder ref"
  assert_file_contains \
    "$test_root/container.log" \
    "container|image delete untagged@${stale_managed_digest}" \
    "builder cleanup did not delete a stale managed dangling builder image"
  assert_file_lacks \
    "$test_root/container.log" \
    "container|image prune" \
    "builder cleanup called global container image prune"
  assert_file_lacks \
    "$test_root/container.log" \
    "container|image delete wrix-profile:old" \
    "builder cleanup deleted a non-builder wrix-managed image"
  assert_file_lacks \
    "$test_root/container.log" \
    "container|image delete user-image:latest" \
    "builder cleanup deleted an unrelated user image"
  assert_file_lacks \
    "$test_root/container.log" \
    "container|image delete untagged@${stale_unlabelled_digest}" \
    "builder cleanup deleted an unlabelled dangling image"
}

test_setup_routes_parses_spaced_apple_network_json() {
  local test_root="$TEST_TMP/setup-routes"

  prepare_builder_fixture "$test_root"

  run_builder "$test_root" setup-routes

  assert_file_contains \
    "$test_root/container.log" \
    "container|network inspect default" \
    "builder route setup did not inspect the Apple default network"
  assert_file_contains \
    "$test_root/container.log" \
    "sudo|route add -net 192.168.64.0/25 -interface bridge100" \
    "builder route setup did not add the lower vmnet split route"
  assert_file_contains \
    "$test_root/container.log" \
    "sudo|route add -net 192.168.64.128/25 -interface bridge100" \
    "builder route setup did not add the upper vmnet split route"
}

test_preserves_existing_private_keys() {
  local test_root="$TEST_TMP/preserve"
  local keys_dir="$test_root/home/.local/share/wrix/builder-keys"
  local first_host_key
  local first_client_key
  local second_host_key
  local second_client_key
  local second_start_output

  require_command ssh-keygen
  require_command base64
  require_command tar
  prepare_builder_fixture "$test_root"

  run_builder "$test_root" start
  first_host_key="$(cat "$keys_dir/host_ed25519")"
  first_client_key="$(cat "$keys_dir/client_ed25519")"

  second_start_output="$(run_builder "$test_root" start)"
  [[ "$second_start_output" == *"Builder container is already running"* ]] \
    || fail "repeated start did not parse the running Apple container state"

  run_builder "$test_root" stop
  run_builder "$test_root" start
  second_host_key="$(cat "$keys_dir/host_ed25519")"
  second_client_key="$(cat "$keys_dir/client_ed25519")"

  [[ "$second_host_key" == "$first_host_key" ]] || fail "host private key was regenerated"
  [[ "$second_client_key" == "$first_client_key" ]] || fail "client private key was regenerated"
  assert_file_contains \
    "$test_root/container.log" \
    "container|stop wrix-builder" \
    "wrix-builder stop did not exercise the fake container lifecycle"
}

run_one() {
  local test_name="$1"
  "$test_name"
  echo "PASS: $test_name"
}

main() {
  if [[ "$#" -gt 0 ]]; then
    run_one "$1"
    return 0
  fi

  run_one test_fake_container_inspect_matches_apple_shape
  run_one test_generates_per_user_ed25519_material
  run_one test_loads_image_through_source_kind_contract
  run_one test_builder_cleanup_is_wrix_scoped
  run_one test_setup_routes_parses_spaced_apple_network_json
  run_one test_preserves_existing_private_keys
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
