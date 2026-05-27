#!/usr/bin/env bash
# Verify wrapix.mkDevShell prekHooks lifecycle (specs/profiles.md § Prek hook management).
#
#   test_auto_set_when_config_present
#     With .pre-commit-config.yaml present and prekHooks defaulted to true,
#     sourcing the shellHook sets core.hooksPath to ${wrapix.prekHooks}.
#
#   test_skip_when_config_absent
#     Without .pre-commit-config.yaml, sourcing the shellHook does NOT set
#     core.hooksPath (lifecycle is gated on the config file's presence).
#
#   test_opt_out
#     With prekHooks = false AND .pre-commit-config.yaml present, sourcing
#     the shellHook does NOT set core.hooksPath (passive opt-out).
#
#   test_derivation_substitute
#     With prekHooks = <custom-derivation>, the shellHook sets core.hooksPath
#     to that derivation's store path when .pre-commit-config.yaml is present.
#
#   test_stale_config_overwrite_with_warning
#     Pre-set core.hooksPath to /some/old/path. With prekHooks=true (default
#     → wrapix.prekHooks) and with prekHooks=<custom-derivation>, the
#     lifecycle overwrites the stale value AND prints a one-line message to
#     stderr naming the old value.
#
#   test_opt_out_preserves_stale_config
#     Pre-set core.hooksPath to /some/old/path, then enter with prekHooks=false.
#     core.hooksPath still equals /some/old/path (passive opt-out by design).
#
# Usage:
#   tests/profiles/mkdevshell-prek.sh                  # run all tests
#   tests/profiles/mkdevshell-prek.sh test_<name>      # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TMP_BASE="$(mktemp -d)"
trap 'rm -rf "$TMP_BASE"' EXIT

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

eval_expr_raw() {
  local expr="$1"
  local system
  system=$(resolve_system)
  nix eval --raw --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\"$system\".lib;
      pkgs = import flake.inputs.nixpkgs { system = \"$system\"; };
    in $expr
  "
}

eval_expr_json() {
  local expr="$1"
  local system
  system=$(resolve_system)
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\"$system\".lib;
      pkgs = import flake.inputs.nixpkgs { system = \"$system\"; };
    in $expr
  "
}

default_bundle_path() {
  eval_expr_raw 'builtins.toString lib.prekHooks'
}

make_repo() {
  local dir
  dir=$(mktemp -d -p "$TMP_BASE")
  ( cd "$dir" && git init -q )
  printf '%s\n' "$dir"
}

get_hooks_path() {
  local dir="$1" val
  if val=$(cd "$dir" && git config --local --get core.hooksPath); then
    printf '%s\n' "$val"
  fi
}

# Source the shellHook in a subshell cd'd to dir; stderr → stderr_file.
# Returns 1 (with a diagnostic) if the subshell exits non-zero.
run_hook_or_fail() {
  local dir="$1" hook="$2" stderr_file="$3"
  local hook_file rc=0
  hook_file=$(mktemp -p "$TMP_BASE")
  printf '%s' "$hook" > "$hook_file"
  # shellcheck source=/dev/null
  ( cd "$dir" && source "$hook_file" ) >/dev/null 2>"$stderr_file" || rc=$?
  if (( rc != 0 )); then
    echo "shellHook source returned exit code $rc; stderr:" >&2
    cat "$stderr_file" >&2
    return 1
  fi
}

mkdevshell_hook() {
  local prek_expr="$1"
  eval_expr_raw "(lib.mkDevShell { profile = lib.profiles.base; prekHooks = ${prek_expr}; }).shellHook"
}

mkdevshell_hook_with_custom_deriv_json() {
  # shellcheck disable=SC2016
  eval_expr_json '
    let
      customHooks = pkgs.runCommand "wrapix-test-prek-hooks" {} '"''"'
        mkdir -p $out
        touch $out/pre-commit
        chmod +x $out/pre-commit
      '"''"';
    in {
      shellHook    = (lib.mkDevShell { profile = lib.profiles.base; prekHooks = customHooks; }).shellHook;
      expectedPath = builtins.toString customHooks;
    }
  '
}

# ============================================================================
test_auto_set_when_config_present() {
  local dir bundle hook stderr_file actual
  bundle=$(default_bundle_path)
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"

  hook=$(mkdevshell_hook "true")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$bundle" ]]; then
    echo "FAIL: expected core.hooksPath='$bundle', got '$actual'" >&2
    return 1
  fi
}

# ============================================================================
test_skip_when_config_absent() {
  local dir hook stderr_file actual
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")

  hook=$(mkdevshell_hook "true")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ -n "$actual" ]]; then
    echo "FAIL: core.hooksPath should be unset without .pre-commit-config.yaml; got '$actual'" >&2
    return 1
  fi
}

# ============================================================================
test_opt_out() {
  local dir hook stderr_file actual
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"

  hook=$(mkdevshell_hook "false")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ -n "$actual" ]]; then
    echo "FAIL: prekHooks=false should not set core.hooksPath; got '$actual'" >&2
    return 1
  fi
}

# ============================================================================
test_derivation_substitute() {
  local dir result hook expected stderr_file actual
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"

  result=$(mkdevshell_hook_with_custom_deriv_json)
  hook=$(jq -r '.shellHook' <<<"$result")
  expected=$(jq -r '.expectedPath' <<<"$result")
  if [[ -z "$expected" ]]; then
    echo "FAIL: custom-derivation eval produced no expectedPath; result was: $result" >&2
    return 1
  fi

  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: expected core.hooksPath='$expected', got '$actual'" >&2
    return 1
  fi
}

# ============================================================================
test_stale_config_overwrite_with_warning() {
  local old_path="/some/old/hooks/path"
  local dir bundle hook stderr_file actual result expected

  # Case 1: prekHooks = true (default → wrapix.prekHooks)
  bundle=$(default_bundle_path)
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"
  ( cd "$dir" && git config --local core.hooksPath "$old_path" )

  hook=$(mkdevshell_hook "true")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed (default-bundle case)" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$bundle" ]]; then
    echo "FAIL: stale path not overwritten with default bundle: expected '$bundle', got '$actual'" >&2
    return 1
  fi
  if ! grep -q "$old_path" "$stderr_file"; then
    echo "FAIL: stderr does not name old path '$old_path' (default-bundle case)" >&2
    cat "$stderr_file" >&2
    return 1
  fi

  # Case 2: prekHooks = <custom-derivation>
  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"
  ( cd "$dir" && git config --local core.hooksPath "$old_path" )

  result=$(mkdevshell_hook_with_custom_deriv_json)
  hook=$(jq -r '.shellHook' <<<"$result")
  expected=$(jq -r '.expectedPath' <<<"$result")
  if [[ -z "$expected" ]]; then
    echo "FAIL: custom-derivation eval produced no expectedPath; result was: $result" >&2
    return 1
  fi

  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed (custom-derivation case)" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: stale path not overwritten with custom derivation: expected '$expected', got '$actual'" >&2
    return 1
  fi
  if ! grep -q "$old_path" "$stderr_file"; then
    echo "FAIL: stderr does not name old path '$old_path' (custom-derivation case)" >&2
    cat "$stderr_file" >&2
    return 1
  fi
}

# ============================================================================
test_opt_out_preserves_stale_config() {
  local old_path="/some/old/hooks/path"
  local dir hook stderr_file actual

  dir=$(make_repo)
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"
  ( cd "$dir" && git config --local core.hooksPath "$old_path" )

  hook=$(mkdevshell_hook "false")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$old_path" ]]; then
    echo "FAIL: prekHooks=false should preserve stale core.hooksPath '$old_path'; got '$actual'" >&2
    return 1
  fi
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_auto_set_when_config_present
  test_skip_when_config_absent
  test_opt_out
  test_derivation_substitute
  test_stale_config_overwrite_with_warning
  test_opt_out_preserves_stale_config
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    if "$fn"; then
      echo "PASS: $fn"
    else
      echo "FAIL: $fn"
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    echo "$failed test(s) failed" >&2
    return 1
  fi
}

if [[ $# -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
