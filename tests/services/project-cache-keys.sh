#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-keys.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

build_wrix() {
  cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wrix-cli --bin wrix || return 1
  printf '%s\n' "$REPO_ROOT/target/debug/wrix"
}

state_root() {
  local roots=("$XDG_STATE_HOME"/wrix/workspaces/*)
  if [[ ! -d "${roots[0]}" ]]; then
    fail "state root was not created"
  fi
  printf '%s\n' "${roots[0]}"
}

cache_root() {
  local roots=("$XDG_CACHE_HOME"/wrix/workspaces/*/binary-cache)
  if [[ ! -d "${roots[0]}" ]]; then
    fail "cache root was not created"
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

write_fake_nix_store() {
  local nix_store="$1"
  cat >"$nix_store" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--generate-binary-cache-key" ]]; then
  key_name="$2"
  secret_path="$3"
  public_path="$4"
  counter_path="${WRIX_FAKE_KEY_COUNTER:?}"
  counter=0
  if [[ -f "$counter_path" ]]; then
    counter="$(<"$counter_path")"
  fi
  counter=$((counter + 1))
  printf '%s\n' "$counter" >"$counter_path"
  public_material="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  if (( counter > 1 )); then
    public_material="AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  fi
  printf '%s-secret-%s\n' "$key_name" "$counter" >"$secret_path"
  printf '%s:%s\n' "$key_name" "$public_material" >"$public_path"
  exit 0
fi

printf 'unsupported fake nix-store command: %s\n' "$*" >&2
exit 2
EOF
  chmod +x "$nix_store"
}

test_rotate_key_wipes_cache() {
  local wrix_bin workspace state cache old_public output new_public
  wrix_bin="$(build_wrix)"
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$TEST_TMP/bin"
  write_fake_nix_store "$TEST_TMP/bin/nix-store"
  export WRIX_NIX_STORE_BIN="$TEST_TMP/bin/nix-store"
  export WRIX_FAKE_KEY_COUNTER="$TEST_TMP/key-counter"
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"

  (cd "$workspace" && "$wrix_bin" service cache status >"$TEST_TMP/status.out")
  state="$(state_root)"
  cache="$(cache_root)"
  old_public="$(cat "$state/keys/cache.pub")"
  printf 'StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root\n' >"$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"
  printf 'old nar\n' >"$cache/nar/old.nar"

  output="$(cd "$workspace" && "$wrix_bin" service cache rotate-key)"
  assert_contains "rotate output" "$output" "rotated project cache key"
  new_public="$(cat "$state/keys/cache.pub")"
  [[ "$new_public" != "$old_public" ]] || fail "public key did not change"
  [[ ! -e "$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ]] || fail "narinfo survived key rotation"
  [[ ! -e "$cache/nar/old.nar" ]] || fail "nar payload survived key rotation"
  [[ -f "$cache/nix-cache-info" ]] || fail "nix-cache-info missing after rotation"
  if grep -qF "$old_public" "$state/keys/cache.pub"; then
    fail "rotated public key still trusts old key text"
  fi
}

ALL_TESTS=(
  test_rotate_key_wipes_cache
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
