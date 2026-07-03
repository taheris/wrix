#!/usr/bin/env bash
# Verify wrix.prekHooks bundle contents (specs/profiles.md § Prek hook management).
#
#   test_bundle_contents
#     lib.prekHooks is a directory derivation containing executable shims for
#     pre-commit, pre-push, prepare-commit-msg, post-checkout, post-merge —
#     and no other paths.
#
#   test_shims_use_hook_impl
#     The materialized pre-commit and pre-push shims invoke
#     `prek hook-impl --hook-type=<stage>` rather than `prek run`.
#
#   test_shims_no_flock
#     No materialized shim sources lock.sh, calls _prek_acquire_lock, or
#     invokes flock; every shim invokes hook-impl and pins the Nix-store
#     prek package on PATH.
#
#   test_pre_push_stamp_written_and_consumed
#     The materialized pre-push shim writes .wrix/push-verified for the
#     current HEAD after a passing pre-push check, then consumes that stamp
#     on a retry of the same HEAD.
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
test_shims_use_hook_impl() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local failed=0
  local hook
  for hook in pre-commit pre-push; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      failed=$((failed + 1))
      continue
    fi
    if grep -qE '^[[:space:]]*[^#].*\bprek run\b' "$bundle/$hook"; then
      echo "FAIL: $hook invokes 'prek run' instead of hook-impl" >&2
      failed=$((failed + 1))
    fi
    if ! grep -qE "hook-impl .*--hook-type=$hook( |$)" "$bundle/$hook"; then
      echo "FAIL: $hook does not invoke 'prek hook-impl --hook-type=$hook'" >&2
      failed=$((failed + 1))
    fi
  done

  [[ "$failed" -eq 0 ]]
}

# ============================================================================
test_shims_no_flock() {
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

# ============================================================================
assert_pre_push_stamp_written_and_consumed() {
  local bundle="$1"
  local worktree="$2"
  local branch="$3"
  local label="$4"

  local failed=0
  local head_sha old_sha ref_line stamp first_out first_err second_out second_err
  head_sha=$(git -C "$worktree" rev-parse HEAD)
  old_sha=0000000000000000000000000000000000000000
  ref_line="refs/heads/$branch $head_sha refs/heads/$branch $old_sha"
  stamp="$worktree/.wrix/push-verified"
  first_out="$worktree/$label-first.out"
  first_err="$worktree/$label-first.err"
  second_out="$worktree/$label-second.out"
  second_err="$worktree/$label-second.err"

  rm -rf "$worktree/.wrix"
  if ! (cd "$worktree" && printf '%s\n' "$ref_line" | "$bundle/pre-push" origin example) >"$first_out" 2>"$first_err"; then
    echo "FAIL: $label first pre-push invocation failed" >&2
    cat "$first_out" >&2
    cat "$first_err" >&2
    failed=$((failed + 1))
  elif [[ ! -f "$stamp" ]]; then
    echo "FAIL: $label pre-push did not write $stamp" >&2
    failed=$((failed + 1))
  elif [[ "$(<"$stamp")" != "$head_sha" ]]; then
    echo "FAIL: $label pre-push stamp did not contain HEAD sha" >&2
    failed=$((failed + 1))
  fi

  if [[ "$failed" -eq 0 ]]; then
    if ! (cd "$worktree" && printf '%s\n' "$ref_line" | "$bundle/pre-push" origin example) >"$second_out" 2>"$second_err"; then
      echo "FAIL: $label stamped pre-push invocation failed" >&2
      cat "$second_out" >&2
      cat "$second_err" >&2
      failed=$((failed + 1))
    elif [[ -e "$stamp" ]]; then
      echo "FAIL: $label pre-push did not consume the matching stamp" >&2
      failed=$((failed + 1))
    elif [[ -s "$second_out" || -s "$second_err" ]]; then
      echo "FAIL: $label stamped pre-push invocation did not short-circuit cleanly" >&2
      cat "$second_out" >&2
      cat "$second_err" >&2
      failed=$((failed + 1))
    fi
  fi

  [[ "$failed" -eq 0 ]]
}

test_pre_push_stamp_written_and_consumed() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work main linked
  work=$(mktemp -d)
  main="$work/main"
  linked="$work/linked"
  mkdir -p "$main"

  local failed=0
  git -C "$main" init -q -b main
  git -C "$main" config user.email test@example.com
  git -C "$main" config user.name Test
  cat >"$main/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: pass
        name: pass
        entry: true
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML
  echo seed >"$main/seed.txt"
  git -C "$main" add .
  git -C "$main" commit -q -m initial

  if ! assert_pre_push_stamp_written_and_consumed "$bundle" "$main" main main-worktree; then
    failed=$((failed + 1))
  fi

  git -C "$main" worktree add -q -b linked "$linked"
  if ! assert_pre_push_stamp_written_and_consumed "$bundle" "$linked" linked linked-worktree; then
    failed=$((failed + 1))
  fi

  rm -rf "$work"
  [[ "$failed" -eq 0 ]]
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_bundle_contents
  test_shims_use_hook_impl
  test_shims_no_flock
  test_pre_push_stamp_written_and_consumed
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
