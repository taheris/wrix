#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-prek.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

build_wrix() {
  if [[ -n "${WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BIN"
    return 0
  fi

  local out_link="$TEST_TMP/wrix"
  nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#wrix"
  printf '%s\n' "$out_link/bin/wrix"
}

current_system() {
  nix eval --raw --impure --expr 'builtins.currentSystem'
}

canonical_dir() {
  local path="$1"
  (cd "$path" && pwd -P)
}

expected_prek_hooks() {
  local path system
  if [[ -n "${WRIX_PREK_HOOKS:-}" ]]; then
    canonical_dir "$WRIX_PREK_HOOKS"
    return 0
  fi

  system="$(current_system)"
  path="$(nix eval --raw "$REPO_ROOT#legacyPackages.$system.lib.prekHooks.outPath")"
  canonical_dir "$path"
}

setup_repo() {
  local name="$1"
  local repo="$TEST_TMP/$name"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.name "Wrix Test"
  git -C "$repo" config user.email "wrix-test@example.invalid"
  git -C "$repo" remote add origin "git@github.com:example/$name.git"
  printf 'repos:\n' >"$repo/.pre-commit-config.yaml"
  printf 'initial\n' >"$repo/README.md"
  git -C "$repo" add README.md .pre-commit-config.yaml
  git -C "$repo" commit -qm "initial"
  printf '%s\n' "$repo"
}

write_deploy_key() {
  local key="$TEST_TMP/deploy-key"
  : >"$key"
  chmod 600 "$key"
  printf '%s\n' "$key"
}

git_common_dir() {
  local repo="$1"
  local common_dir
  common_dir="$(git -C "$repo" rev-parse --git-common-dir)"
  if [[ "$common_dir" != /* ]]; then
    common_dir="$repo/$common_dir"
  fi
  canonical_dir "$common_dir"
}

core_hooks_path() {
  local repo="$1"
  git -C "$repo" config --get core.hooksPath
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

assert_equals() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: got '$actual', expected '$expected'"
  fi
}

assert_common_config_origin() {
  local label="$1"
  local repo="$2"
  local common_dir origin
  common_dir="$(git_common_dir "$repo")"
  origin="$(git -C "$repo" config --show-origin --get core.hooksPath)"
  if [[ "$origin" == *"file:$common_dir/config"* || "$origin" == file:.git/config* ]]; then
    return 0
  fi
  fail "$label: hooksPath was not read from common config $common_dir/config: $origin"
}

add_integration_worktree() {
  local repo="$1"
  local integration="$repo/.loom/integration"
  mkdir -p "$repo/.loom"
  git -C "$repo" -c core.hooksPath=/dev/null worktree add -q "$integration" -b loom-integration
  printf '%s\n' "$integration"
}

run_init() {
  local repo="$1"
  local deploy_key="$2"
  shift 2
  (cd "$repo" && HOME="$TEST_TMP/home" WRIX_DEPLOY_KEY="$deploy_key" "$@")
}

test_prek_hooks() {
  local wrix_bin expected_hooks deploy_key repo output actual integration sentinel config_repo flag_repo
  wrix_bin="$(build_wrix)"
  expected_hooks="$(expected_prek_hooks)"
  deploy_key="$(write_deploy_key)"

  repo="$(setup_repo prek-enabled)"
  output="$(run_init "$repo" "$deploy_key" "$wrix_bin" init --offline --no-sign --key prek-key)"
  assert_contains "enabled init output" "$output" "prek_hooks: true"
  actual="$(core_hooks_path "$repo")"
  assert_equals "enabled hooksPath" "$actual" "$expected_hooks"
  assert_common_config_origin "enabled hooks origin" "$repo"

  integration="$(add_integration_worktree "$repo")"
  actual="$(core_hooks_path "$integration")"
  assert_equals "linked hooksPath" "$actual" "$expected_hooks"
  assert_common_config_origin "linked hooks origin" "$integration"

  sentinel="legacy-hooks"

  flag_repo="$(setup_repo prek-disabled-flag)"
  git -C "$flag_repo" config core.hooksPath "$sentinel"
  output="$(run_init "$flag_repo" "$deploy_key" "$wrix_bin" init --offline --no-sign --no-hooks --key prek-key)"
  assert_contains "flag disabled output" "$output" "prek_hooks: false"
  actual="$(core_hooks_path "$flag_repo")"
  assert_equals "flag disabled hooksPath" "$actual" "$sentinel"

  config_repo="$(setup_repo prek-disabled-config)"
  git -C "$config_repo" config core.hooksPath "$sentinel"
  cat >"$config_repo/wrix.toml" <<'TOML'
[wrix.init]
prek_hooks = false
TOML
  output="$(run_init "$config_repo" "$deploy_key" "$wrix_bin" init --offline --no-sign --key prek-key)"
  assert_contains "config disabled output" "$output" "prek_hooks: false"
  actual="$(core_hooks_path "$config_repo")"
  assert_equals "config disabled hooksPath" "$actual" "$sentinel"
}

ALL_TESTS=(
  test_prek_hooks
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
