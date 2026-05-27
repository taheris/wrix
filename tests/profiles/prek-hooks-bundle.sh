#!/usr/bin/env bash
# Verify wrapix.prekHooks bundle contents (specs/profiles.md § Prek hook management).
#
#   test_bundle_contents
#     lib.prekHooks is a directory derivation containing executable shims for
#     pre-commit, pre-push, prepare-commit-msg, post-checkout, post-merge plus
#     a _lib/lock.sh helper.
#
#   test_flock_only_on_fr2_stages
#     pre-commit and pre-push shims source _lib/lock.sh and call
#     _prek_acquire_lock; prepare-commit-msg, post-checkout, post-merge call
#     prek directly without sourcing the lock helper (FR2 stages only — the
#     informational hooks do not contend for the index).
#
# Usage:
#   tests/profiles/prek-hooks-bundle.sh                  # run all tests
#   tests/profiles/prek-hooks-bundle.sh test_<name>      # run a single test

set -euo pipefail

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

# ============================================================================
# Bundle contains all expected shims plus the lock helper, with executable
# bits on the shims (lock helper is library-style, not directly runnable).
# ============================================================================
test_bundle_contents() {
  local bundle
  if ! bundle=$(bundle_path); then
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

  if [[ ! -f "$bundle/_lib/lock.sh" ]]; then
    echo "FAIL: bundle missing _lib/lock.sh helper" >&2
    missing=$((missing + 1))
  fi

  [[ "$missing" -eq 0 ]]
}

# ============================================================================
# pre-commit and pre-push source _lib/lock.sh and call _prek_acquire_lock.
# The informational hooks (prepare-commit-msg, post-checkout, post-merge) do
# NOT source the lock helper — they run after-the-fact and never contend.
# ============================================================================
test_flock_only_on_fr2_stages() {
  local bundle
  if ! bundle=$(bundle_path); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local failed=0
  local hook
  for hook in pre-commit pre-push; do
    if ! grep -q 'source.*_lib/lock\.sh' "$bundle/$hook"; then
      echo "FAIL: $hook does not source _lib/lock.sh" >&2
      failed=$((failed + 1))
    fi
    if ! grep -q '_prek_acquire_lock' "$bundle/$hook"; then
      echo "FAIL: $hook does not call _prek_acquire_lock" >&2
      failed=$((failed + 1))
    fi
  done

  for hook in prepare-commit-msg post-checkout post-merge; do
    if grep -q '_lib/lock\.sh' "$bundle/$hook"; then
      echo "FAIL: $hook sources lock helper (should call prek directly)" >&2
      failed=$((failed + 1))
    fi
    if grep -q '_prek_acquire_lock' "$bundle/$hook"; then
      echo "FAIL: $hook calls _prek_acquire_lock (should call prek directly)" >&2
      failed=$((failed + 1))
    fi
  done

  [[ "$failed" -eq 0 ]]
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_bundle_contents
  test_flock_only_on_fr2_stages
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
