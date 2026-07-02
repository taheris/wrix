#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-config.XXXXXX)"
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

expected_source_kind() {
  case "$(uname -s)" in
    Darwin) printf 'docker-archive\n' ;;
    *) printf 'nix-descriptor\n' ;;
  esac
}

setup_repo() {
  local name="$1"
  local repo="$TEST_TMP/$name"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" remote add origin "git@github.com:example/$name.git"
  printf 'repos:\n' >"$repo/.pre-commit-config.yaml"
  printf '%s\n' "$repo"
}

write_profile_config() {
  local path="$1"
  local source_kind
  source_kind="$(expected_source_kind)"
  cat >"$path" <<JSON
{"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"localhost/wrix-test:latest","source":"/nix/store/fake-image","source_kind":"$source_kind","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":"profile-key"},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":true}},"features":{"mcp_runtime":false}}
JSON
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

assert_policy_line() {
  local label="$1"
  local output="$2"
  local key="$3"
  local value="$4"
  assert_contains "$label" "$output" "$key: $value"
}

run_init() {
  local repo="$1"
  shift
  (cd "$repo" && HOSTNAME=devhost "$@")
}

test_defaults_and_overrides() {
  local wrix_bin repo profile_config output
  wrix_bin="$(build_wrix)"
  repo="$(setup_repo config-defaults)"
  profile_config="$TEST_TMP/profile-config.json"
  write_profile_config "$profile_config"

  output="$(run_init "$repo" "$wrix_bin" init)"
  assert_policy_line "derived defaults" "$output" "deploy_key" "config-defaults-devhost"
  assert_policy_line "derived defaults" "$output" "sign_commits" "true"
  assert_policy_line "derived defaults" "$output" "remote" "origin"
  assert_policy_line "derived defaults" "$output" "prek_hooks" "true"
  assert_policy_line "derived defaults" "$output" "online_verify" "true"
  if [[ -e "$repo/wrix.toml" ]]; then
    fail "wrix init created wrix.toml for default behavior"
  fi

  output="$(run_init "$repo" "$wrix_bin" --profile-config "$profile_config" init)"
  assert_policy_line "profile config" "$output" "deploy_key" "profile-key"
  assert_policy_line "profile config" "$output" "remote" "origin"

  git -C "$repo" remote add upstream "git@github.com:example/upstream.git"
  cat >"$repo/wrix.toml" <<'TOML'
[wrix.git]
deploy_key = "toml-key"
sign_commits = false
remote = "upstream"

[wrix.init]
prek_hooks = false
online_verify = false
TOML
  output="$(run_init "$repo" "$wrix_bin" --profile-config "$profile_config" init)"
  assert_policy_line "wrix.toml" "$output" "deploy_key" "toml-key"
  assert_policy_line "wrix.toml" "$output" "sign_commits" "false"
  assert_policy_line "wrix.toml" "$output" "remote" "upstream"
  assert_policy_line "wrix.toml" "$output" "prek_hooks" "false"
  assert_policy_line "wrix.toml" "$output" "online_verify" "false"

  cat >"$repo/wrix.toml" <<'TOML'
[wrix.git]
deploy_key = "toml-key"
sign_commits = true
remote = "upstream"

[wrix.init]
prek_hooks = true
online_verify = true
TOML
  output="$(run_init "$repo" "$wrix_bin" --profile-config "$profile_config" init --key flag-key --remote origin --offline --no-sign --no-hooks --force)"
  assert_policy_line "flag overrides" "$output" "deploy_key" "flag-key"
  assert_policy_line "flag overrides" "$output" "sign_commits" "false"
  assert_policy_line "flag overrides" "$output" "remote" "origin"
  assert_policy_line "flag overrides" "$output" "prek_hooks" "false"
  assert_policy_line "flag overrides" "$output" "online_verify" "false"
  assert_policy_line "flag overrides" "$output" "force" "true"
}

ALL_TESTS=(
  test_defaults_and_overrides
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
