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

  mkdir -p "$bin_dir"

  cat >"$bin_dir/container" <<'CONTAINER'
#!/usr/bin/env bash
set -euo pipefail

log_file="${WRIX_BUILDER_FAKE_LOG:?}"
state_dir="${WRIX_BUILDER_FAKE_STATE:?}"
tar_root="${WRIX_BUILDER_FAKE_TAR_ROOT:?}"
mkdir -p "$state_dir/containers"
printf 'container|%s\n' "$*" >>"$log_file"

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
  image)
    case "${2:-}" in
      inspect | delete | tag | prune)
        exit 0
        ;;
      load)
        printf 'Loaded image: untagged@sha256:%064d\n' 1
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
      printf '{"status":"%s"}\n' "$status"
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

  cat >"$bin_dir/sw_vers" <<'SWVERS'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-productVersion" ]]; then
  printf '26.0\n'
  exit 0
fi

printf 'fake sw_vers: unexpected args: %s\n' "$*" >&2
exit 64
SWVERS
  chmod +x "$bin_dir/sw_vers"

  cat >"$bin_dir/sleep" <<'SLEEP'
#!/usr/bin/env bash
set -euo pipefail

exit 0
SLEEP
  chmod +x "$bin_dir/sleep"
}

prepare_builder_fixture() {
  local test_root="$1"

  mkdir -p "$test_root/home" "$test_root/state" "$test_root/tar-root/nix/store"
  printf '%s\n' "seed" >"$test_root/tar-root/nix/store/seed"
  : >"$test_root/container.log"
  write_fake_tools "$test_root/bin"
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

test_preserves_existing_private_keys() {
  local test_root="$TEST_TMP/preserve"
  local keys_dir="$test_root/home/.local/share/wrix/builder-keys"
  local first_host_key
  local first_client_key
  local second_host_key
  local second_client_key

  require_command ssh-keygen
  require_command base64
  require_command tar
  prepare_builder_fixture "$test_root"

  run_builder "$test_root" start
  first_host_key="$(cat "$keys_dir/host_ed25519")"
  first_client_key="$(cat "$keys_dir/client_ed25519")"

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

  run_one test_generates_per_user_ed25519_material
  run_one test_preserves_existing_private_keys
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
