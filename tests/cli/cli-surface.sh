#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-cli-surface.XXXXXX)"
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

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

assert_executable() {
  local path="$1"
  if [[ ! -x "$path" ]]; then
    fail "expected executable at $path"
  fi
}

assert_no_legacy_binaries() {
  local wrix_bin="$1"
  local bin_dir
  bin_dir="$(dirname "$wrix_bin")"

  local forbidden
  for forbidden in beads-dolt beads-push wrix-svc; do
    if [[ -e "$bin_dir/$forbidden" ]]; then
      fail "wrix package exposes forbidden binary $forbidden"
    fi
  done

  local repo_beads_bin
  for repo_beads_bin in "$bin_dir"/*-beads; do
    if [[ -e "$repo_beads_bin" ]]; then
      fail "wrix package exposes forbidden repo-beads binary $repo_beads_bin"
    fi
  done
}

setup_repo() {
  local name="$1"
  local repo="$TEST_TMP/$name"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" remote add origin "git@github.com:example/$name.git"
  printf '%s\n' "$repo"
}

git_config_snapshot() {
  local repo="$1"
  git -C "$repo" config --local --list --show-origin
}

expect_failure() {
  local repo="$1"
  local output_file="$2"
  shift 2
  if (cd "$repo" && "$@") >"$output_file" 2>&1; then
    fail "expected command to fail: $*"
  fi
}

test_root_help_and_legacy_binaries() {
  local wrix_bin
  wrix_bin="$(build_wrix)"
  assert_executable "$wrix_bin"

  local root_help help_command init_help run_help spawn_help service_help beads_help
  root_help="$("$wrix_bin" --help)"
  help_command="$("$wrix_bin" help)"
  init_help="$("$wrix_bin" init --help)"
  run_help="$("$wrix_bin" run --help)"
  spawn_help="$("$wrix_bin" spawn --help)"
  service_help="$("$wrix_bin" service --help)"
  beads_help="$("$wrix_bin" beads --help)"

  local command
  for command in run spawn service beads init; do
    assert_contains "root help" "$root_help" "$command"
    assert_contains "help command" "$help_command" "$command"
  done

  assert_contains "run help" "$run_help" "Usage: wrix"
  assert_contains "spawn help" "$spawn_help" "Usage: wrix"
  assert_contains "service help" "$service_help" "Commands:"
  assert_contains "beads help" "$beads_help" "push"
  assert_contains "init help" "$init_help" "Usage: wrix init"
  assert_contains "init help" "$init_help" "--deploy"
  assert_contains "init help" "$init_help" "--offline"

  assert_no_legacy_binaries "$wrix_bin"
}

test_help_errors_are_non_mutating() {
  local wrix_bin repo before after output_file output
  wrix_bin="$(build_wrix)"
  repo="$(setup_repo help-errors)"
  output_file="$TEST_TMP/output.txt"

  before="$(git_config_snapshot "$repo")"
  output="$(cd "$repo" && "$wrix_bin" init --help)"
  after="$(git_config_snapshot "$repo")"
  assert_contains "init help" "$output" "Usage: wrix init"
  if [[ "$before" != "$after" ]]; then
    fail "wrix init --help mutated git config"
  fi
  if [[ -e "$repo/wrix.toml" ]]; then
    fail "wrix init --help created wrix.toml"
  fi

  if "$wrix_bin" not-a-command >"$output_file" 2>&1; then
    fail "unknown root command unexpectedly succeeded"
  fi
  output="$(<"$output_file")"
  assert_contains "unknown command" "$output" "unknown command: not-a-command"
  assert_contains "unknown command" "$output" "Usage: wrix <command>"

  before="$(git_config_snapshot "$repo")"
  expect_failure "$repo" "$output_file" "$wrix_bin" init --deploy --offline
  after="$(git_config_snapshot "$repo")"
  output="$(<"$output_file")"
  assert_contains "deploy offline" "$output" "--deploy cannot be used with --offline"
  assert_contains "deploy offline" "$output" "Usage: wrix init"
  if [[ "$before" != "$after" ]]; then
    fail "wrix init --deploy --offline mutated git config"
  fi

  cat >"$repo/wrix.toml" <<'TOML'
[wrix.init]
online_verify = false
TOML
  before="$(git_config_snapshot "$repo")"
  expect_failure "$repo" "$output_file" "$wrix_bin" init --deploy
  after="$(git_config_snapshot "$repo")"
  output="$(<"$output_file")"
  assert_contains "deploy offline policy" "$output" "--deploy requires online verification"
  assert_contains "deploy offline policy" "$output" "Usage: wrix init"
  if [[ "$before" != "$after" ]]; then
    fail "wrix init --deploy under offline policy mutated git config"
  fi
}

ALL_TESTS=(
  test_root_help_and_legacy_binaries
  test_help_errors_are_non_mutating
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
