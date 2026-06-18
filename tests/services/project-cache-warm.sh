#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-warm.XXXXXX)"
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

if [[ "${1:-}" == "build" ]]; then
  printf '%s\n' "$*" >>"${WRIX_FAKE_BUILD_LOG:?}"
  exit 0
fi

if [[ "${1:-}" == "path-info" && "${2:-}" == "--json" ]]; then
  case "${3:-}" in
    .#packages.x86_64-linux.pkg)
      printf '{"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg":{}}\n'
      ;;
    .#devShells.x86_64-linux.default)
      printf '{"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-devshell":{}}\n'
      ;;
    .#checks.x86_64-linux.check)
      printf '{"/nix/store/cccccccccccccccccccccccccccccccc-check":{}}\n'
      ;;
    *)
      printf 'unexpected path-info installable: %s\n' "${3:-}" >&2
      exit 2
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "copy" ]]; then
  printf '%s\n' "$*" >>"${WRIX_FAKE_COPY_LOG:?}"
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
    printf '%s\n' "$path"
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
  export WRIX_FAKE_BUILD_LOG="$TEST_TMP/build.log"
  export WRIX_FAKE_COPY_LOG="$TEST_TMP/copy.log"
  export WRIX_UPSTREAM_SUBSTITUTERS=""
  : >"$WRIX_FAKE_NIX_LOG"
  : >"$WRIX_FAKE_BUILD_LOG"
  : >"$WRIX_FAKE_COPY_LOG"
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
      "name": "devShells.x86_64-linux.default",
      "installable": ".#devShells.x86_64-linux.default",
      "drv_path": "/nix/store/22222222222222222222222222222222-devshell.drv"
    },
    {
      "name": "checks.x86_64-linux.check",
      "installable": ".#checks.x86_64-linux.check",
      "drv_path": "/nix/store/33333333333333333333333333333333-check.drv"
    }
  ]
}
JSON
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

test_fake_warm_tools_contract() {
  with_fake_tools
  local build_output path_info
  "$WRIX_NIX_BIN" build --no-link .#packages.x86_64-linux.pkg .#devShells.x86_64-linux.default
  build_output="$(cat "$WRIX_FAKE_BUILD_LOG")"
  assert_contains "fake build" "$build_output" ".#packages.x86_64-linux.pkg"
  assert_contains "fake build" "$build_output" ".#devShells.x86_64-linux.default"
  path_info="$($WRIX_NIX_BIN path-info --json .#checks.x86_64-linux.check)"
  assert_contains "fake path-info" "$path_info" "/nix/store/cccccccccccccccccccccccccccccccc-check"
}

test_warm_roots() {
  local wrix_bin roots_file workspace default_build checks_build state_roots
  wrix_bin="$(build_wrix)"
  with_fake_tools
  with_workspace_env
  roots_file="$TEST_TMP/roots.json"
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"
  write_roots_file "$roots_file"
  export WRIX_CACHE_ROOTS_FILE="$roots_file"

  (cd "$workspace" && "$wrix_bin" service cache warm >"$TEST_TMP/warm-default.out")
  default_build="$(cat "$WRIX_FAKE_BUILD_LOG")"
  assert_contains "default warm build" "$default_build" ".#packages.x86_64-linux.pkg"
  assert_contains "default warm build" "$default_build" ".#devShells.x86_64-linux.default"
  assert_not_contains "default warm build" "$default_build" ".#checks.x86_64-linux.check"

  : >"$WRIX_FAKE_BUILD_LOG"
  (cd "$workspace" && "$wrix_bin" service cache warm --checks >"$TEST_TMP/warm-checks.out")
  checks_build="$(cat "$WRIX_FAKE_BUILD_LOG")"
  assert_contains "checks warm build" "$checks_build" ".#packages.x86_64-linux.pkg"
  assert_contains "checks warm build" "$checks_build" ".#devShells.x86_64-linux.default"
  assert_contains "checks warm build" "$checks_build" ".#checks.x86_64-linux.check"

  state_roots=("$XDG_STATE_HOME"/wrix/workspaces/*/gcroots)
  [[ -f "${state_roots[0]}/packages.x86_64-linux.pkg" ]] || fail "package marker missing after warm"
  [[ -f "${state_roots[0]}/devShells.x86_64-linux.default" ]] || fail "devShell marker missing after warm"
  [[ -f "${state_roots[0]}/checks.x86_64-linux.check" ]] || fail "check marker missing after --checks warm"
}

ALL_TESTS=(
  test_fake_warm_tools_contract
  test_warm_roots
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
