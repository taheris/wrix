#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-status.XXXXXX)"
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
  cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wrix-cli --bin wrix
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

test_warn_size() {
  local wrix_bin workspace state cache output
  wrix_bin="$(build_wrix)"
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"

  (cd "$workspace" && "$wrix_bin" service cache status >"$TEST_TMP/initial.out")
  state="$(state_root)"
  cache="$(cache_root)"
  mkdir -p "$state/pending" "$state/gcroots"
  local now
  now="$(date +%s)"
  cat >"$state/pending/pending.json" <<JSON
{
  "created_at_epoch": $now,
  "drv_path": "/nix/store/11111111111111111111111111111111-root.drv",
  "out_paths": ["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root"]
}
JSON
  cat >"$state/cache-status.json" <<'JSON'
{
  "dirty": true,
  "last_publish": "ok",
  "last_prune": "ok",
  "last_error": "previous warning",
  "last_prune_epoch": 1
}
JSON
  cat >"$state/services.json" <<'JSON'
{
  "endpoints": {
    "cache_http": { "host": "127.0.0.1", "port": 21001 }
  }
}
JSON
  printf '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root\n' >"$state/gcroots/root"
  printf 'StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root\n' >"$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"
  printf 'payload larger than the one byte threshold\n' >"$cache/nar/payload.nar"

  output="$(cd "$workspace" && WRIX_CACHE_SOFT_LIMIT_BYTES=1 "$wrix_bin" service cache status)"
  assert_contains "status output" "$output" "cache size:"
  assert_contains "status output" "$output" "pending records: 1"
  assert_contains "status output" "$output" "dirty: true"
  assert_contains "status output" "$output" "last_publish: ok"
  assert_contains "status output" "$output" "last_prune: ok"
  assert_contains "status output" "$output" "last_error: previous warning"
  assert_contains "status output" "$output" "cache_http"
  assert_contains "status output" "$output" "warning: project cache size exceeds 1 byte soft threshold"
  [[ -f "$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ]] || fail "status deleted a reachable narinfo"
}

ALL_TESTS=(
  test_warn_size
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
