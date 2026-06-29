#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-prune.XXXXXX)"
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

write_fake_nix() {
  local nix="$1"
  cat >"$nix" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

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
        printf 'payload for %s\n' "$arg" >"$cache_root/nar/$hash.nar"
        printf 'StorePath: %s\nURL: nar/%s.nar\n' "$arg" "$hash" >"$cache_root/$hash.narinfo"
        ;;
    esac
  done
  exit 0
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
  local out_path="$2"
  cat >"$roots_file" <<JSON
{
  "roots": [
    {
      "name": "packages.x86_64-linux.root",
      "installable": ".#packages.x86_64-linux.root",
      "drv_path": "/nix/store/11111111111111111111111111111111-root.drv",
      "out_paths": ["$out_path"]
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

test_root_marker_replacement_prunes_stale_outputs() {
  local wrix_bin workspace roots_file state cache marker
  wrix_bin="$(build_wrix)"
  with_fake_tools
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  workspace="$TEST_TMP/workspace"
  roots_file="$TEST_TMP/roots.json"
  mkdir -p "$workspace"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  write_roots_file "$roots_file" "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-old-root"
  (cd "$workspace" && "$wrix_bin" service cache publish >"$TEST_TMP/old.out")
  cache="$(cache_root)"
  [[ -f "$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ]] || fail "old narinfo missing after first publish"
  [[ -f "$cache/nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar" ]] || fail "old nar payload missing after first publish"
  printf 'orphan payload\n' >"$cache/nar/orphan.nar"

  write_roots_file "$roots_file" "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-new-root"
  (cd "$workspace" && "$wrix_bin" service cache publish >"$TEST_TMP/new.out")
  state="$(state_root)"
  marker="$state/gcroots/packages.x86_64-linux.root"
  [[ -f "$cache/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.narinfo" ]] || fail "new narinfo missing after second publish"
  [[ -f "$cache/nar/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.nar" ]] || fail "new nar payload missing after second publish"
  [[ ! -e "$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ]] || fail "stale old narinfo survived prune"
  [[ ! -e "$cache/nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar" ]] || fail "stale old nar payload survived prune"
  [[ ! -e "$cache/nar/orphan.nar" ]] || fail "orphan nar payload survived prune"
  [[ -f "$marker" ]] || fail "root marker missing"
  if grep -q 'old-root' "$marker"; then
    fail "root marker still contains stale output"
  fi
  grep -q 'new-root' "$marker" || fail "root marker does not contain current output"
}

ALL_TESTS=(
  test_root_marker_replacement_prunes_stale_outputs
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
