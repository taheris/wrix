#!/usr/bin/env bash
set -euo pipefail

TEST_TMP="$(mktemp -d -t wrix-builder-keys.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"

if ! command -v ssh-keygen >/dev/null 2>&1; then
  echo "SKIP: ssh-keygen is required" >&2
  exit 77
fi

if ! command -v base64 >/dev/null 2>&1; then
  echo "SKIP: base64 is required" >&2
  exit 77
fi

source "$REPO_ROOT/lib/builder/keys.sh"

fail() {
  local message="$1"
  echo "FAIL: $message" >&2
  exit 1
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

  read -r key_type _ < "$path"
  [[ "$key_type" == "ssh-ed25519" ]] || fail "$path is $key_type, expected ssh-ed25519"
}

test_generates_per_user_ed25519_material() {
  local keys_dir="$TEST_TMP/generate/wrix/builder-keys"
  local decoded="$TEST_TMP/host-key.pub"

  wrix_builder_ensure_key_material "$keys_dir" "2222"

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
  assert_mode "$keys_dir/known_hosts" "600"

  assert_ed25519_public_key "$keys_dir/host_ed25519.pub"
  assert_ed25519_public_key "$keys_dir/client_ed25519.pub"

  base64_decode "$keys_dir/public_host_key_base64" > "$decoded"
  cmp "$keys_dir/host_ed25519.pub" "$decoded" >/dev/null \
    || fail "base64 host-key metadata does not decode to the host public key"

  grep -Fxq "[localhost]:2222 $(cat "$keys_dir/host_ed25519.pub")" "$keys_dir/known_hosts" \
    || fail "known_hosts does not pin the generated host key"
}

test_preserves_existing_private_keys() {
  local keys_dir="$TEST_TMP/preserve/wrix/builder-keys"
  local first_host_key
  local first_client_key
  local second_host_key
  local second_client_key

  wrix_builder_ensure_key_material "$keys_dir" "2222"
  first_host_key="$(cat "$keys_dir/host_ed25519")"
  first_client_key="$(cat "$keys_dir/client_ed25519")"

  wrix_builder_ensure_key_material "$keys_dir" "2222"
  second_host_key="$(cat "$keys_dir/host_ed25519")"
  second_client_key="$(cat "$keys_dir/client_ed25519")"

  [[ "$second_host_key" == "$first_host_key" ]] || fail "host private key was regenerated"
  [[ "$second_client_key" == "$first_client_key" ]] || fail "client private key was regenerated"
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
