#!/usr/bin/env bash
# Verify wrapix.prekHooks bundle contents (specs/profiles.md § Prek hook management).
#
#   test_bundle_contents
#     lib.prekHooks is a directory derivation containing executable shims for
#     pre-commit, pre-push, prepare-commit-msg, post-checkout, post-merge —
#     and no other paths.
#
#   test_shims_are_plain_hook_impl
#     Every shim invokes `prek hook-impl --hook-type=<its-stage>` and none
#     source lock.sh, call _prek_acquire_lock, or invoke flock — the bundle
#     no longer wraps any stage in a serialized critical section. Every shim
#     also pins the Nix-store prek package on PATH so git hooks work outside
#     the devShell.
#
# Usage:
#   tests/profiles/prek-hooks-bundle.sh                  # run all tests
#   tests/profiles/prek-hooks-bundle.sh test_<name>      # run a single test

set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

bundle_path() {
  local system
  system=$(resolve_system)
  nix build --no-link --print-out-paths --no-warn-dirty \
    "$REPO_ROOT#legacyPackages.$system.lib.prekHooks"
}

require_bundle() {
  if [[ $# -gt 0 ]]; then
    printf '%s\n' "$1"
    return 0
  fi
  bundle_path
}

# ============================================================================
# Bundle contains the five shims (executable) and no _lib/ subdirectory.
# ============================================================================
test_bundle_contents() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local missing=0
  local hook
  for hook in pre-commit pre-push prepare-commit-msg post-checkout post-merge; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      missing=$((missing + 1))
      continue
    fi
    if [[ ! -x "$bundle/$hook" ]]; then
      echo "FAIL: bundle shim not executable: $hook" >&2
      missing=$((missing + 1))
    fi
  done

  local found
  found=$(find "$bundle" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)
  local expected=$'post-checkout\npost-merge\npre-commit\npre-push\nprepare-commit-msg'
  if [[ "$found" != "$expected" ]]; then
    echo "FAIL: bundle contains unexpected paths:" >&2
    printf '%s\n' "$found" >&2
    missing=$((missing + 1))
  fi

  [[ "$missing" -eq 0 ]]
}

# ============================================================================
# Every shim is a plain hook-impl invocation: no lock.sh source, no
# _prek_acquire_lock call, no flock invocation; and each shim names its own
# stage via --hook-type=<stage>.
# ============================================================================
test_shims_are_plain_hook_impl() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local failed=0
  local hook
  for hook in pre-commit pre-push prepare-commit-msg post-checkout post-merge; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      failed=$((failed + 1))
      continue
    fi
    if grep -qE 'lock\.sh' "$bundle/$hook"; then
      echo "FAIL: $hook still references lock.sh" >&2
      failed=$((failed + 1))
    fi
    if grep -q '_prek_acquire_lock' "$bundle/$hook"; then
      echo "FAIL: $hook still calls _prek_acquire_lock" >&2
      failed=$((failed + 1))
    fi
    if grep -qE '\bflock\b' "$bundle/$hook"; then
      echo "FAIL: $hook still invokes flock" >&2
      failed=$((failed + 1))
    fi
    if ! grep -qE "hook-impl .*--hook-type=$hook( |$)" "$bundle/$hook"; then
      echo "FAIL: $hook does not invoke 'prek hook-impl --hook-type=$hook'" >&2
      failed=$((failed + 1))
    fi
    if ! grep -qE '/nix/store/[^"]+-prek-[^/]+/bin' "$bundle/$hook"; then
      echo "FAIL: $hook does not pin prek on PATH" >&2
      failed=$((failed + 1))
    fi
  done

  [[ "$failed" -eq 0 ]]
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_bundle_contents
  test_shims_are_plain_hook_impl
)

run_all() {
  local failed=0
  local bundle
  if ! bundle=$(bundle_path); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local fn
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    if "$fn" "$bundle"; then
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
