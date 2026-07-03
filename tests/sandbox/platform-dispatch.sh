#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-platform-dispatch.XXXXXX)"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

skip() {
  local reason="$1"
  printf 'SKIP: %s\n' "$reason" >&2
  exit 77
}

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  command -v "$command_name" >/dev/null 2>&1 || skip "$command_name not on PATH"
}

expected_source_kind() {
  case "$(uname -s)" in
    Darwin) printf 'docker-archive\n' ;;
    Linux) printf 'nix-descriptor\n' ;;
    *) skip "unsupported platform for sandbox dispatch verifier: $(uname -s)" ;;
  esac
}

test_platform_dispatch_current_system() {
  local expected result
  require_command nix
  require_command jq

  expected="$(expected_source_kind)"
  result=$(nix eval --impure --no-warn-dirty --json --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
      sandbox = lib.mkSandbox { profile = lib.profiles.base; };
    in
    {
      sourceKind = sandbox.image.source_kind;
      mainProgram = sandbox.package.meta.mainProgram or \"\";
      launcherIsWrix = sandbox.launcher == flake.packages.\${system}.wrix;
    }
  ")

  if ! jq -e --arg expected "$expected" '
    .sourceKind == $expected and
    .mainProgram == "wrix-run" and
    .launcherIsWrix == true
  ' <<<"$result" >/dev/null; then
    fail "mkSandbox did not dispatch to the current platform implementation: $result"
  fi

  printf 'PASS: mkSandbox dispatches current platform image/source-kind and launcher metadata\n' >&2
}

test_unsupported_system_error() {
  local out_file err_file rc
  require_command nix

  out_file="$TEST_TMP/unsupported.out"
  err_file="$TEST_TMP/unsupported.err"
  set +e
  nix eval --impure --no-warn-dirty --expr "
    let
      root = \"$REPO_ROOT\";
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      current = builtins.currentSystem;
      linuxSystem =
        if builtins.match \".*-linux\" current != null then
          current
        else
          builtins.replaceStrings [ \"darwin\" ] [ \"linux\" ] current;
      pkgs = flake.inputs.nixpkgs.legacyPackages.\${current};
      linuxPkgs = flake.inputs.nixpkgs.legacyPackages.\${linuxSystem};
      unsupported = import (root + \"/lib\") {
        inherit pkgs linuxPkgs;
        system = \"riscv64-linux\";
        inherit (flake.inputs) crane fenix;
        treefmt = flake.formatter.\${current};
      };
    in
    (unsupported.mkSandbox { profile = unsupported.profiles.base; }).launcher
  " >"$out_file" 2>"$err_file"
  rc="$?"
  set -e

  if [[ "$rc" -eq 0 ]]; then
    fail "unsupported-system mkSandbox evaluation unexpectedly succeeded: $(<"$out_file")"
  fi
  if ! grep -qF 'Unsupported system: riscv64-linux' "$err_file"; then
    fail "unsupported-system error did not name the unsupported system: $(<"$err_file")"
  fi

  printf 'PASS: mkSandbox errors at evaluation on unsupported systems\n' >&2
}

ALL_TESTS=(
  test_platform_dispatch_current_system
  test_unsupported_system_error
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
  [[ "$failed" -eq 0 ]]
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
