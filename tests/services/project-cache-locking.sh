#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-locking.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

build_bins() {
  cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wrix-cli --bin wrix -p wrix-cache --bin wrix-cache-publish
}

require_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP: python3 is required for lock contention assertions\n' >&2
    exit 77
  fi
}

write_fake_nix() {
  local nix="$1"
  cat >"$nix" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "path-info" && "${2:-}" == "--json" ]]; then
  printf '{"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root":{}}\n'
  exit 0
fi

if [[ "${1:-}" == "copy" ]]; then
  previous=""
  cache_root=""
  for arg in "$@"; do
    if [[ "$previous" == "--to" ]]; then
      cache_root="${arg#file://}"
    fi
    previous="$arg"
  done
  mkdir -p "$cache_root/nar"
  for arg in "$@"; do
    case "$arg" in
      /nix/store/*)
        base="${arg##*/}"
        hash="${base%%-*}"
        printf 'StorePath: %s\n' "$arg" >"$cache_root/$hash.narinfo"
        ;;
    esac
  done
  exit 0
fi

if [[ "${1:-}" == "path-info" && "${2:-}" == "--store" ]]; then
  exit 1
fi

printf 'unsupported fake nix command: %s\n' "$*" >&2
exit 2
EOF
  chmod +x "$nix"
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
  printf '%s-secret\n' "$key_name" >"$secret_path"
  printf '%s:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n' "$key_name" >"$public_path"
  exit 0
fi

if [[ "${1:-}" == "--query" && "${2:-}" == "--requisites" ]]; then
  shift 2
  printf '%s\n' "$@"
  exit 0
fi

printf 'unsupported fake nix-store command: %s\n' "$*" >&2
exit 2
EOF
  chmod +x "$nix_store"
}

with_fake_tools() {
  local bin_dir="$TEST_TMP/bin"
  mkdir -p "$bin_dir"
  write_fake_nix "$bin_dir/nix"
  write_fake_nix_store "$bin_dir/nix-store"
  export WRIX_NIX_BIN="$bin_dir/nix"
  export WRIX_NIX_STORE_BIN="$bin_dir/nix-store"
  export WRIX_UPSTREAM_SUBSTITUTERS=""
}

write_roots_file() {
  local roots_file="$1"
  cat >"$roots_file" <<'JSON'
{
  "roots": [
    {
      "name": "packages.x86_64-linux.root",
      "installable": ".#packages.x86_64-linux.root",
      "drv_path": "/nix/store/11111111111111111111111111111111-root.drv"
    }
  ]
}
JSON
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

test_pending_on_lock_timeout() {
  require_python
  build_bins
  with_fake_tools
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  local workspace="$TEST_TMP/workspace"
  local roots_file="$TEST_TMP/roots.json"
  mkdir -p "$workspace"
  write_roots_file "$roots_file"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  (cd "$workspace" && "$REPO_ROOT/target/debug/wrix" service cache publish >"$TEST_TMP/initial.out")
  local state cache workspace_hash output pending
  state="$(state_root)"
  cache="$(cache_root)"
  workspace_hash="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  local lock_ready="$TEST_TMP/lock.ready"
  local lock_release="$TEST_TMP/lock.release"
  local lock_pid
  local attempt
  python3 - "$state/cache.lock" "$lock_ready" "$lock_release" <<'PY' &
import fcntl
import pathlib
import sys
import time

lock_path = pathlib.Path(sys.argv[1])
ready_path = pathlib.Path(sys.argv[2])
release_path = pathlib.Path(sys.argv[3])
with lock_path.open("a+", encoding="utf-8") as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    ready_path.write_text("ready\n", encoding="utf-8")
    while not release_path.exists():
        time.sleep(0.05)
PY
  lock_pid="$!"
  attempt=0
  while [[ "$attempt" -lt 100 ]]; do
    if [[ -f "$lock_ready" ]]; then
      break
    fi
    attempt=$((attempt + 1))
    sleep 0.05
  done
  [[ -f "$lock_ready" ]] || fail "cache.lock holder did not acquire the lock"

  output="$(WRIX_CACHE_LOCK_TIMEOUT_MS=1 "$REPO_ROOT/target/debug/wrix-cache-publish" \
    --workspace-hash "$workspace_hash" \
    --state-root "$state" \
    --cache-root "$cache" \
    --manifest "$state/publish-roots.json" \
    --drv-path /nix/store/11111111111111111111111111111111-root.drv \
    --out-paths /nix/store/dddddddddddddddddddddddddddddddd-pending)"
  assert_contains "publisher output" "$output" "waiting for project cache lock"
  assert_contains "publisher output" "$output" "recorded pending publish"
  pending=("$state"/pending/*.json)
  [[ -f "${pending[0]}" ]] || fail "pending record was not written"
  assert_contains "pending record" "$(cat "${pending[0]}")" "/nix/store/dddddddddddddddddddddddddddddddd-pending"

  : >"$lock_release"
  wait "$lock_pid"
  (cd "$workspace" && "$REPO_ROOT/target/debug/wrix" service cache publish >"$TEST_TMP/drain.out")
  if compgen -G "$state/pending/*.json" >/dev/null; then
    fail "matching pending record was not drained"
  fi
  assert_contains "drain output" "$(cat "$TEST_TMP/drain.out")" "drained pending"
}

ALL_TESTS=(
  test_pending_on_lock_timeout
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
