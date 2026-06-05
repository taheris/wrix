#!/usr/bin/env bash
# Verify wrix.mkDevShell composition rules (specs/profiles.md § mkDevShell).
#
#   test_profile_required
#     mkDevShell {} (no profile) errors at evaluation — no two-arg fallback.
#
#   test_profile_shellhook_spliced
#     mkDevShell { profile = profiles.rust; } shellHook contains the rust
#     profile's exports (RUSTC_WRAPPER, SCCACHE_DIR, SCCACHE_CACHE_SIZE,
#     CARGO_INCREMENTAL).
#
#   test_packages_merge
#     mkDevShell { profile = base; packages = [extra]; }.packages contains
#     both profile.packages and extra (profile.packages ++ packages).
#
#   test_env_right_merge
#     mkDevShell { profile; env = { K = "v"; }; } sets K=v on the resulting
#     shell derivation. When the key collides with profile.env (e.g.
#     CARGO_INCREMENTAL on the rust profile), consumer wins.
#
#   test_shellhook_order
#     mkDevShell { profile = profiles.rust; shellHook = "MARKER_XYZ"; }
#     shellHook contains both the rust profile's exports AND the consumer
#     marker, with the consumer marker appearing AFTER the profile exports.
#
# Usage:
#   tests/profiles/mkdevshell.sh                  # run all tests
#   tests/profiles/mkdevshell.sh test_<name>      # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

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

# ============================================================================
# mkDevShell requires `profile` — no default, no two-arg fallback.
# ============================================================================
test_profile_required() {
  if eval_expr_raw "(lib.mkDevShell {}).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: lib.mkDevShell {} should error at evaluation (missing profile)" >&2
    return 1
  fi
}

# ============================================================================
# rust profile shellHook is spliced verbatim into the devshell shellHook.
# ============================================================================
test_profile_shellhook_spliced() {
  local hook
  if ! hook=$(eval_expr_raw "(lib.mkDevShell { profile = lib.profiles.rust; }).shellHook"); then
    echo "FAIL: mkDevShell { profile = profiles.rust; } evaluation failed" >&2
    return 1
  fi

  local missing=0
  local marker
  for marker in 'RUSTC_WRAPPER' 'SCCACHE_DIR' 'SCCACHE_CACHE_SIZE' 'CARGO_INCREMENTAL'; do
    if ! grep -q "$marker" <<<"$hook"; then
      echo "FAIL: rust profile shellHook missing export of $marker" >&2
      missing=$((missing + 1))
    fi
  done
  [[ "$missing" -eq 0 ]]
}

# ============================================================================
# packages = profile.packages ++ packages
# Uses two extras of different counts so we can verify the merge is append
# rather than replace, without forcing package element evaluation.
# ============================================================================
test_packages_merge() {
  local result
  if ! result=$(eval_expr_json "
    let
      base = lib.mkDevShell { profile = lib.profiles.base; };
      ext1 = lib.mkDevShell {
        profile  = lib.profiles.base;
        packages = [ pkgs.hello ];
      };
      ext3 = lib.mkDevShell {
        profile  = lib.profiles.base;
        packages = [ pkgs.hello pkgs.jq pkgs.curl ];
      };
    in {
      baseLen = builtins.length base.nativeBuildInputs;
      ext1Len = builtins.length ext1.nativeBuildInputs;
      ext3Len = builtins.length ext3.nativeBuildInputs;
    }
  "); then
    echo "FAIL: nix eval mkDevShell packages-merge expression failed" >&2
    return 1
  fi

  local base_len ext1_len ext3_len
  base_len=$(echo "$result" | jq -r '.baseLen')
  ext1_len=$(echo "$result" | jq -r '.ext1Len')
  ext3_len=$(echo "$result" | jq -r '.ext3Len')

  if (( base_len < 1 )); then
    echo "FAIL: base devshell has no packages — profile.packages was not appended" >&2
    return 1
  fi
  if (( ext1_len != base_len + 1 )); then
    echo "FAIL: adding 1 package should grow nativeBuildInputs by 1 (base=$base_len, ext1=$ext1_len)" >&2
    return 1
  fi
  if (( ext3_len != base_len + 3 )); then
    echo "FAIL: adding 3 packages should grow nativeBuildInputs by 3 (base=$base_len, ext3=$ext3_len)" >&2
    return 1
  fi
}

# ============================================================================
# env = profile.env // env (right-biased; consumer wins on conflict)
# ============================================================================
test_env_right_merge() {
  local result
  if ! result=$(eval_expr_json "
    let
      added = lib.mkDevShell {
        profile = lib.profiles.base;
        env     = { MKDEVSHELL_TEST_KEY = \"mkdevshell_test_value\"; };
      };
      conflict = lib.mkDevShell {
        profile = lib.profiles.rust;
        env     = { CARGO_INCREMENTAL = \"1\"; };
      };
    in {
      addedValue        = added.MKDEVSHELL_TEST_KEY or null;
      conflictValue     = conflict.CARGO_INCREMENTAL or null;
    }
  "); then
    echo "FAIL: nix eval mkDevShell env-merge expression failed" >&2
    return 1
  fi

  local added_value conflict_value
  added_value=$(echo "$result"    | jq -r '.addedValue')
  conflict_value=$(echo "$result" | jq -r '.conflictValue')

  if [[ "$added_value" != "mkdevshell_test_value" ]]; then
    echo "FAIL: env.MKDEVSHELL_TEST_KEY expected 'mkdevshell_test_value', got '$added_value'" >&2
    return 1
  fi
  if [[ "$conflict_value" != "1" ]]; then
    echo "FAIL: consumer env.CARGO_INCREMENTAL=1 should override rust profile's '0', got '$conflict_value'" >&2
    return 1
  fi
}

# ============================================================================
# shellHook order: lifecycle → profile.shellHook → consumer shellHook
# ============================================================================
test_shellhook_order() {
  local marker="WRIX_MKDEVSHELL_CONSUMER_MARKER_XYZ"
  local hook
  if ! hook=$(eval_expr_raw "
    (lib.mkDevShell {
      profile   = lib.profiles.rust;
      shellHook = \"echo $marker\";
    }).shellHook
  "); then
    echo "FAIL: mkDevShell shellHook-order evaluation failed" >&2
    return 1
  fi

  if ! grep -q "$marker" <<<"$hook"; then
    echo "FAIL: consumer marker '$marker' missing from devshell shellHook" >&2
    return 1
  fi

  local rustc_line marker_line
  rustc_line=$(grep -n 'RUSTC_WRAPPER' <<<"$hook" | head -1 | cut -d: -f1)
  marker_line=$(grep -n "$marker"      <<<"$hook" | head -1 | cut -d: -f1)

  if [[ -z "$rustc_line" ]]; then
    echo "FAIL: rust profile RUSTC_WRAPPER export missing from shellHook" >&2
    return 1
  fi
  if [[ -z "$marker_line" ]]; then
    echo "FAIL: consumer marker line not found" >&2
    return 1
  fi
  if (( marker_line <= rustc_line )); then
    echo "FAIL: consumer shellHook (line $marker_line) must appear AFTER profile.shellHook (RUSTC_WRAPPER on line $rustc_line)" >&2
    return 1
  fi
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_profile_required
  test_profile_shellhook_spliced
  test_packages_merge
  test_env_right_merge
  test_shellhook_order
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
