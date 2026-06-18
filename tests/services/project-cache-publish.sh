#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-publish.XXXXXX)"
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

printf 'nix %s\n' "$*" >>"${WRIX_FAKE_NIX_LOG:?}"

if [[ "${1:-}" == "path-info" && "${2:-}" == "--json" ]]; then
  case "${3:-}" in
    .#packages.x86_64-linux.pkg)
      printf '{"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg":{}}\n'
      ;;
    .#checks.x86_64-linux.check)
      printf 'missing check\n' >&2
      exit 1
      ;;
    *)
      printf 'unexpected path-info installable: %s\n' "${3:-}" >&2
      exit 2
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "path-info" && "${2:-}" == "--store" ]]; then
  if [[ "${4:-}" == "/nix/store/cccccccccccccccccccccccccccccccc-upstream-dep" ]]; then
    printf '%s\n' "${4:-}"
    exit 0
  fi
  exit 1
fi

if [[ "${1:-}" == "copy" ]]; then
  printf '%s\n' "$*" >"${WRIX_FAKE_COPY_LOG:?}"
  cache_root=""
  previous=""
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
        printf 'StorePath: %s\nSig: fake-signature\n' "$arg" >"$cache_root/$hash.narinfo"
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

printf 'nix-store %s\n' "$*" >>"${WRIX_FAKE_NIX_LOG:?}"

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
  for path in "$@"; do
    case "$path" in
      /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg)
        printf '%s\n' "$path"
        printf '%s\n' "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-local-dep"
        printf '%s\n' "/nix/store/cccccccccccccccccccccccccccccccc-upstream-dep"
        ;;
      /nix/store/dddddddddddddddddddddddddddddddd-pending)
        printf '%s\n' "$path"
        ;;
      *)
        printf '%s\n' "$path"
        ;;
    esac
  done
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
  export WRIX_FAKE_NIX_LOG="$TEST_TMP/nix.log"
  export WRIX_FAKE_COPY_LOG="$TEST_TMP/copy.log"
  export WRIX_UPSTREAM_SUBSTITUTERS="https://cache.example.invalid"
  : >"$WRIX_FAKE_NIX_LOG"
}

with_workspace_env() {
  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/state"
  export XDG_CACHE_HOME="$TEST_TMP/cache"
  rm -rf "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
}

write_roots_file() {
  local roots_file="$1"
  cat >"$roots_file" <<'JSON'
{
  "roots": [
    {
      "name": "packages.x86_64-linux.pkg",
      "installable": ".#packages.x86_64-linux.pkg",
      "drv_path": "/nix/store/11111111111111111111111111111111-pkg.drv"
    },
    {
      "name": "checks.x86_64-linux.check",
      "installable": ".#checks.x86_64-linux.check",
      "drv_path": "/nix/store/22222222222222222222222222222222-check.drv"
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

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label unexpectedly contained '$needle'"
  fi
}

test_fake_publish_tools_contract() {
  with_fake_tools
  local path_info copy_output closure_output
  path_info="$($WRIX_NIX_BIN path-info --json .#packages.x86_64-linux.pkg)"
  assert_contains "fake path-info" "$path_info" "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg"
  closure_output="$($WRIX_NIX_STORE_BIN --query --requisites /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg)"
  assert_contains "fake closure" "$closure_output" "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-local-dep"
  mkdir -p "$TEST_TMP/cache-contract"
  "$WRIX_NIX_BIN" copy --to "file://$TEST_TMP/cache-contract" --no-recursive --secret-key-files "$TEST_TMP/key" /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg
  copy_output="$(cat "$WRIX_FAKE_COPY_LOG")"
  assert_contains "fake copy" "$copy_output" "--no-recursive"
  [[ -f "$TEST_TMP/cache-contract/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo" ]] || fail "fake copy did not write narinfo"
}

test_realized_only_publish() {
  local wrix_bin workspace roots_file output state pending
  wrix_bin="$(build_wrix)"
  with_fake_tools
  with_workspace_env
  workspace="$TEST_TMP/workspace-realized"
  roots_file="$TEST_TMP/roots.json"
  mkdir -p "$workspace"
  write_roots_file "$roots_file"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  output="$(cd "$workspace" && "$wrix_bin" service cache publish)"
  state="$(state_root)"
  pending="$state/pending/record.json"
  cat >"$pending" <<'JSON'
{
  "drv_path": "/nix/store/11111111111111111111111111111111-pkg.drv",
  "out_paths": ["/nix/store/dddddddddddddddddddddddddddddddd-pending"]
}
JSON
  output="$output
$(cd "$workspace" && "$wrix_bin" service cache publish)"

  [[ ! -e "$pending" ]] || fail "matching pending record was not drained"
  [[ -f "$state/gcroots/packages.x86_64-linux.pkg" ]] || fail "GC marker was not written"
  assert_contains "publish output" "$output" "unrealized root: .#checks.x86_64-linux.check"
  assert_contains "copy command" "$(cat "$WRIX_FAKE_COPY_LOG")" "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg"
  assert_contains "copy command" "$(cat "$WRIX_FAKE_COPY_LOG")" "/nix/store/dddddddddddddddddddddddddddddddd-pending"
  assert_not_contains "nix log" "$(cat "$WRIX_FAKE_NIX_LOG")" "nix build"
}

test_project_scope_filter() {
  local wrix_bin workspace roots_file copied
  wrix_bin="$(build_wrix)"
  with_fake_tools
  with_workspace_env
  workspace="$TEST_TMP/workspace-scope"
  roots_file="$TEST_TMP/roots-scope.json"
  mkdir -p "$workspace"
  write_roots_file "$roots_file"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  (cd "$workspace" && "$wrix_bin" service cache publish >"$TEST_TMP/scope.out")
  copied="$(cat "$WRIX_FAKE_COPY_LOG")"
  assert_contains "copy command" "$copied" "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg"
  assert_contains "copy command" "$copied" "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-local-dep"
  assert_not_contains "copy command" "$copied" "/nix/store/cccccccccccccccccccccccccccccccc-upstream-dep"
  assert_not_contains "copy command" "$copied" "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-arbitrary-host-path"
}

test_flat_cache_signed_no_recursive() {
  local wrix_bin workspace roots_file copied cache narinfo
  wrix_bin="$(build_wrix)"
  with_fake_tools
  with_workspace_env
  workspace="$TEST_TMP/workspace-flat"
  roots_file="$TEST_TMP/roots-flat.json"
  mkdir -p "$workspace"
  write_roots_file "$roots_file"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  (cd "$workspace" && "$wrix_bin" service cache publish >"$TEST_TMP/flat.out")
  copied="$(cat "$WRIX_FAKE_COPY_LOG")"
  cache="$(cache_root)"
  narinfo="$cache/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"
  assert_contains "copy command" "$copied" "--no-recursive"
  assert_contains "copy command" "$copied" "--secret-key-files"
  assert_contains "copy command" "$copied" "file://$cache"
  [[ -f "$narinfo" ]] || fail "narinfo was not written"
  assert_contains "narinfo" "$(cat "$narinfo")" "Sig:"
}

ALL_TESTS=(
  test_fake_publish_tools_contract
  test_realized_only_publish
  test_project_scope_filter
  test_flat_cache_signed_no_recursive
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
