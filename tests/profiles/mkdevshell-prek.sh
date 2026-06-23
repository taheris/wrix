#!/usr/bin/env bash
# Verify wrix.mkDevShell prekHooks lifecycle (specs/profiles.md § Prek hook management).
#
#   test_auto_set_when_config_present
#     With .pre-commit-config.yaml present and prekHooks defaulted to true,
#     sourcing the shellHook sets core.hooksPath to ${wrix.prekHooks}.
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
#     → wrix.prekHooks) and with prekHooks=<custom-derivation>, the
#     lifecycle overwrites the stale value AND prints a one-line message to
#     stderr naming the old value.
#
#   test_opt_out_preserves_stale_config
#     Pre-set core.hooksPath to /some/old/path, then enter with prekHooks=false.
#     core.hooksPath still equals /some/old/path (passive opt-out by design).
#
#   test_wrappers_exposed_and_on_devshell_path
#     pre-push-checks and skip-if-missing are exported from the flake library
#     and included in mkDevShell's package set.
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

make_linked_worktree() {
  local primary worktree branch
  primary=$(make_repo)
  git -C "$primary" config user.email "wrix-test@example.invalid"
  git -C "$primary" config user.name "Wrix Test"
  : > "$primary/README.md"
  git -C "$primary" add README.md
  git -C "$primary" commit -q -m "initial commit"
  worktree=$(mktemp -d -p "$TMP_BASE")
  rmdir "$worktree"
  branch="wrix-test-$(basename "$worktree" | tr -c '[:alnum:]' '-')"
  git -C "$primary" worktree add -q -b "$branch" "$worktree"
  printf '%s\n' "$worktree"
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
  local hook_file service_bin host_config rc=0
  hook_file=$(mktemp -p "$TMP_BASE")
  service_bin=$(mktemp -p "$TMP_BASE")
  host_config=$(mktemp -p "$TMP_BASE")
  cat > "$host_config" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
exit 0
SCRIPT
  chmod +x "$host_config"
  sed -E "s|/nix/store/[[:alnum:]]+-wrix-host-nix-config\.sh|$host_config|g" <<<"$hook" > "$hook_file"
  cat > "$service_bin" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "service" && "${2:-}" = "start" ]]; then
  exit 0
fi
if [[ "${1:-}" = "service" && "${2:-}" = "endpoints" ]]; then
  printf '%s\n' '{"cache_http":null}'
  exit 0
fi
echo "fake wrix: unexpected args: $*" >&2
exit 2
SCRIPT
  chmod +x "$service_bin"
  # shellcheck source=/dev/null
  ( cd "$dir" && WRIX_BIN="$service_bin" source "$hook_file" ) >/dev/null 2>"$stderr_file" || rc=$?
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
      customHooks = pkgs.runCommand "wrix-test-prek-hooks" {} '"''"'
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

assert_auto_set_hooks_path() {
  local dir="$1"
  local bundle="$2"
  local label="$3"
  local hook stderr_file actual
  stderr_file=$(mktemp -p "$TMP_BASE")
  : > "$dir/.pre-commit-config.yaml"

  hook=$(mkdevshell_hook "true")
  if ! run_hook_or_fail "$dir" "$hook" "$stderr_file"; then
    echo "FAIL: shellHook source failed for $label" >&2
    return 1
  fi

  actual=$(get_hooks_path "$dir")
  if [[ "$actual" != "$bundle" ]]; then
    echo "FAIL: $label expected core.hooksPath='$bundle', got '$actual'" >&2
    return 1
  fi
}

# ============================================================================
test_auto_set_when_config_present() {
  local bundle
  bundle=$(default_bundle_path)

  assert_auto_set_hooks_path "$(make_repo)" "$bundle" "regular repository" || return 1
  assert_auto_set_hooks_path "$(make_linked_worktree)" "$bundle" "linked worktree" || return 1
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

  # Case 1: prekHooks = true (default → wrix.prekHooks)
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

# ============================================================================
test_wrappers_exposed_and_on_devshell_path() {
  local result
  result=$(eval_expr_json '
    let
      shell = lib.mkDevShell { profile = lib.profiles.base; };
      packageNames = map (drv: drv.pname or drv.name or (builtins.toString drv)) shell.nativeBuildInputs;
    in {
      hasPrePushChecks = lib ? prePushChecks;
      hasSkipIfMissing = lib ? skipIfMissing;
      prePushChecksPath = if lib ? prePushChecks then builtins.toString lib.prePushChecks else "";
      skipIfMissingPath = if lib ? skipIfMissing then builtins.toString lib.skipIfMissing else "";
      inherit packageNames;
    }
  ')

  local failed=0
  if [[ "$(jq -r '.hasPrePushChecks' <<<"$result")" != "true" ]]; then
    echo "FAIL: flake legacyPackages.lib does not expose prePushChecks" >&2
    failed=$((failed + 1))
  fi
  if [[ "$(jq -r '.hasSkipIfMissing' <<<"$result")" != "true" ]]; then
    echo "FAIL: flake legacyPackages.lib does not expose skipIfMissing" >&2
    failed=$((failed + 1))
  fi
  if [[ -z "$(jq -r '.prePushChecksPath' <<<"$result")" ]]; then
    echo "FAIL: prePushChecks did not evaluate to a store path" >&2
    failed=$((failed + 1))
  fi
  if [[ -z "$(jq -r '.skipIfMissingPath' <<<"$result")" ]]; then
    echo "FAIL: skipIfMissing did not evaluate to a store path" >&2
    failed=$((failed + 1))
  fi
  if ! jq -e 'any(.packageNames[]; . == "pre-push-checks")' <<<"$result" >/dev/null; then
    echo "FAIL: mkDevShell nativeBuildInputs lacks pre-push-checks" >&2
    failed=$((failed + 1))
  fi
  if ! jq -e 'any(.packageNames[]; . == "skip-if-missing")' <<<"$result" >/dev/null; then
    echo "FAIL: mkDevShell nativeBuildInputs lacks skip-if-missing" >&2
    failed=$((failed + 1))
  fi

  [[ "$failed" -eq 0 ]]
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_auto_set_when_config_present
  test_skip_when_config_absent
  test_opt_out
  test_derivation_substitute
  test_stale_config_overwrite_with_warning
  test_opt_out_preserves_stale_config
  test_wrappers_exposed_and_on_devshell_path
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
