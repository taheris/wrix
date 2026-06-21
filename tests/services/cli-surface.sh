#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cli.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

build_package() {
  local package="$1"
  local out_link="$TEST_TMP/$package"
  if ! nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#$package"; then
    return 1
  fi
  printf '%s\n' "$out_link"
}

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

expected_source_kind() {
  case "$(uname -s)" in
    Darwin) printf 'docker-archive\n' ;;
    *) printf 'nix-descriptor\n' ;;
  esac
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

assert_package_attr_absent() {
  local attr="$1"
  local out_file="$TEST_TMP/$attr.out"
  local err_file="$TEST_TMP/$attr.err"
  if nix build --no-link --no-warn-dirty "$REPO_ROOT#$attr" >"$out_file" 2>"$err_file"; then
    fail "legacy package attr is still exposed: $attr"
  fi
}

test_wrix_service_cli() {
  local package
  package="$(build_package wrix)" || return 1
  local wrix_bin="$package/bin/wrix"
  assert_executable "$wrix_bin"

  local root_help service_help dolt_help cache_help beads_help
  root_help="$($wrix_bin --help)"
  service_help="$($wrix_bin service --help)"
  dolt_help="$($wrix_bin service dolt --help)"
  cache_help="$($wrix_bin service cache --help)"
  beads_help="$($wrix_bin beads --help)"

  assert_contains "root help" "$root_help" "service"
  assert_contains "root help" "$root_help" "beads"

  for command in start stop status logs endpoints; do
    assert_contains "service help" "$service_help" "$command"
  done

  for command in status socket port host attach gc; do
    assert_contains "dolt help" "$dolt_help" "$command"
  done

  for command in status publish warm prune rotate-key; do
    assert_contains "cache help" "$cache_help" "$command"
  done

  assert_contains "beads help" "$beads_help" "push"

  for forbidden in beads-dolt beads-push wrix-svc; do
    if [[ -e "$package/bin/$forbidden" ]]; then
      fail "wrix package exposes forbidden binary $forbidden"
    fi
  done

  local repo_beads_bin
  for repo_beads_bin in "$package"/bin/*-beads; do
    if [[ -e "$repo_beads_bin" ]]; then
      fail "wrix package exposes forbidden repo-beads binary $repo_beads_bin"
    fi
  done
}

test_wrix_spawn_delegates_to_sandbox_launcher() {
  local package profile_config spawn_config output
  package="$(build_package wrix)" || return 1
  profile_config="$TEST_TMP/profile-config.json"
  local source_kind
  source_kind=$(expected_source_kind)
  cat >"$profile_config" <<JSON
{"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"localhost/wrix-test:latest","source":"/nix/store/fake-image","source_kind":"$source_kind","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":true}},"features":{"mcp_runtime":false}}
JSON
  spawn_config="$TEST_TMP/spawn.json"
  cat >"$spawn_config" <<JSON
{
  "workspace": "$REPO_ROOT",
  "image_ref": "localhost/wrix-test:latest",
  "image_source": "",
  "env": [],
  "agent_args": ["echo", "hello"],
  "mounts": []
}
JSON

  output="$(WRIX_DRY_RUN=1 "$package/bin/wrix" --profile-config "$profile_config" spawn --spawn-config "$spawn_config" --stdio)"
  assert_contains "spawn dry run" "$output" "SUBCOMMAND=spawn"
  assert_contains "spawn dry run" "$output" "STDIO=1"
  if [[ "$output" == *"unavailable in this build"* ]]; then
    fail "wrix spawn still points at the Rust stub"
  fi
}

test_rust_helper_binaries() {
  local wrix_package hook_package publish_package serve_package
  wrix_package="$(build_package wrix)" || return 1
  hook_package="$(build_package wrix-cache-hook)" || return 1
  publish_package="$(build_package wrix-cache-publish)" || return 1
  serve_package="$(build_package wrix-cache-serve)" || return 1

  assert_executable "$wrix_package/bin/wrix"
  assert_executable "$hook_package/bin/wrix-cache-hook"
  assert_executable "$publish_package/bin/wrix-cache-publish"
  assert_executable "$serve_package/bin/wrix-cache-serve"

  local hook_help publish_help serve_help
  hook_help="$("$hook_package"/bin/wrix-cache-hook --help)"
  publish_help="$("$publish_package"/bin/wrix-cache-publish --help)"
  serve_help="$("$serve_package"/bin/wrix-cache-serve --help)"

  assert_contains "hook helper help" "$hook_help" "Usage: wrix-cache-hook"
  assert_contains "publish helper help" "$publish_help" "Usage: wrix-cache-publish"
  assert_contains "serve helper help" "$serve_help" "Usage: wrix-cache-serve"

  assert_package_attr_absent beads-dolt
  assert_package_attr_absent beads-push
  assert_package_attr_absent wrix-svc
}

ALL_TESTS=(
  test_wrix_service_cli
  test_wrix_spawn_delegates_to_sandbox_launcher
  test_rust_helper_binaries
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
