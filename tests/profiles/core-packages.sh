#!/usr/bin/env bash
# Verify the corePackages tier-1 membership key on the profile attrset
# produced by lib/sandbox/profiles.nix.
#
# corePackages is the wrix-controlled, fixed-per-instance package set.
# Downstream extension grows `packages` only, never `corePackages`, so the
# leaf delta an image rebuilds on is `packages` − `corePackages`.
#
#   test_base_core_equals_packages
#     The base profile has no leaf delta: corePackages == packages (every
#     base package is fixed per instance) and corePackages is non-empty.
#
#   test_rust_toolchain_in_core
#     The rust profile's toolchain + fixed support packages (gcc, openssl,
#     pkg-config, sccache, ...) are corePackages, not leaf: rust corePackages
#     strictly exceeds base corePackages, cargo-nextest is leaf tooling, and
#     sccache is a member of corePackages.
#
#   test_python_extras_not_core
#     The python profile keeps corePackages == base (basePackages only); its
#     toolchain extras (ruff, ty, uv) live in packages as tier-2 leaf content.
#
#   test_extension_grows_packages_only
#     deriveProfile appends to packages, never corePackages: an extension with
#     one extra package leaves corePackages unchanged and produces a one-element
#     leaf delta.
#
#   test_pinned_toolchain_in_core
#     A downstream-pinned rustProfile { toolchain; sha256; packages = [p]; }
#     lands its pinned toolchain in corePackages (tier 1) while cargo-nextest
#     and the extension package become leaf: corePackages exceeds base,
#     leaf delta == 2.
#
# Usage:
#   tests/profiles/core-packages.sh                 # run all tests
#   tests/profiles/core-packages.sh test_<name>     # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.75.0).
# Update this if the fixture's channel/components change.
TOOLCHAIN_FIXTURE_SHA="sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8="

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

# Evaluate $1 (a Nix expression) as JSON against the live flake. The expression
# may reference: `wlib` (wrix lib with profiles/deriveProfile/rustProfile),
# `np` (nixpkgs package set), and `lib` (nixpkgs lib). Helper bindings `leaf`
# and `coreLen`/`pkgsLen`/`leafLen` are also in scope.
eval_profile_json() {
  local expr="$1"
  local system
  system=$(resolve_system)
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = \"$system\";
      wlib = flake.legacyPackages.\${system}.lib;
      np = flake.inputs.nixpkgs.legacyPackages.\${system};
      lib = np.lib;
      coreOuts = prof: map (p: p.outPath) prof.corePackages;
      leaf = prof: builtins.filter (p: !(builtins.elem p.outPath (coreOuts prof))) prof.packages;
      coreLen = prof: builtins.length prof.corePackages;
      pkgsLen = prof: builtins.length prof.packages;
      leafLen = prof: builtins.length (leaf prof);
    in $expr
  "
}

fail() {
  echo "FAIL: $1" >&2
  return 1
}

# ============================================================================
test_base_core_equals_packages() {
  local result
  result=$(eval_profile_json "
    let p = wlib.profiles.base; in {
      core = coreLen p;
      pkgs = pkgsLen p;
      leaf = leafLen p;
    }
  ")
  local core pkgs leaf
  core=$(echo "$result" | jq -r '.core')
  pkgs=$(echo "$result" | jq -r '.pkgs')
  leaf=$(echo "$result" | jq -r '.leaf')

  [[ "$core" -gt 0 ]] || fail "base corePackages should be non-empty, got $core"
  [[ "$leaf" -eq 0 ]] || fail "base leaf delta should be empty, got $leaf"
  [[ "$core" -eq "$pkgs" ]] || fail "base corePackages ($core) should equal packages ($pkgs)"
}

# ============================================================================
test_rust_toolchain_in_core() {
  local result
  result=$(eval_profile_json "
    {
      baseCore = coreLen wlib.profiles.base;
      rustCore = coreLen wlib.profiles.rust;
      rustLeaf = leafLen wlib.profiles.rust;
      hasSccache = builtins.any (p: lib.hasInfix \"sccache\" p.outPath) wlib.profiles.rust.corePackages;
      hasNextestLeaf = builtins.any (p: lib.hasInfix \"cargo-nextest\" p.outPath) (leaf wlib.profiles.rust);
      hasNextestCore = builtins.any (p: lib.hasInfix \"cargo-nextest\" p.outPath) wlib.profiles.rust.corePackages;
    }
  ")
  local base_core rust_core rust_leaf has_sccache has_nextest_leaf has_nextest_core
  base_core=$(echo "$result" | jq -r '.baseCore')
  rust_core=$(echo "$result" | jq -r '.rustCore')
  rust_leaf=$(echo "$result" | jq -r '.rustLeaf')
  has_sccache=$(echo "$result" | jq -r '.hasSccache')
  has_nextest_leaf=$(echo "$result" | jq -r '.hasNextestLeaf')
  has_nextest_core=$(echo "$result" | jq -r '.hasNextestCore')

  [[ "$rust_core" -gt "$base_core" ]] \
    || fail "rust corePackages ($rust_core) should exceed base ($base_core) — toolchain belongs in core"
  [[ "$rust_leaf" -eq 1 ]] || fail "rust leaf delta should contain only cargo-nextest, got $rust_leaf"
  [[ "$has_sccache" == "true" ]] || fail "sccache should be a member of rust corePackages"
  [[ "$has_nextest_leaf" == "true" ]] || fail "cargo-nextest should be rust leaf tooling"
  [[ "$has_nextest_core" == "false" ]] || fail "cargo-nextest should not be a member of rust corePackages"
}

# ============================================================================
test_python_extras_not_core() {
  local result
  result=$(eval_profile_json "
    {
      baseCore = coreLen wlib.profiles.base;
      pyCore = coreLen wlib.profiles.python;
      pyLeaf = leafLen wlib.profiles.python;
    }
  ")
  local base_core py_core py_leaf
  base_core=$(echo "$result" | jq -r '.baseCore')
  py_core=$(echo "$result" | jq -r '.pyCore')
  py_leaf=$(echo "$result" | jq -r '.pyLeaf')

  [[ "$py_core" -eq "$base_core" ]] \
    || fail "python corePackages ($py_core) should equal base ($base_core) — python extras are tier-2 leaf"
  [[ "$py_leaf" -gt 0 ]] \
    || fail "python should carry tier-2 leaf packages (ruff/ty/uv), got leaf $py_leaf"
}

# ============================================================================
test_extension_grows_packages_only() {
  local result
  result=$(eval_profile_json "
    let ext = wlib.deriveProfile wlib.profiles.base { packages = [ np.hello ]; }; in {
      baseCore = coreLen wlib.profiles.base;
      basePkgs = pkgsLen wlib.profiles.base;
      extCore = coreLen ext;
      extPkgs = pkgsLen ext;
      extLeaf = leafLen ext;
    }
  ")
  local base_core base_pkgs ext_core ext_pkgs ext_leaf
  base_core=$(echo "$result" | jq -r '.baseCore')
  base_pkgs=$(echo "$result" | jq -r '.basePkgs')
  ext_core=$(echo "$result" | jq -r '.extCore')
  ext_pkgs=$(echo "$result" | jq -r '.extPkgs')
  ext_leaf=$(echo "$result" | jq -r '.extLeaf')

  [[ "$ext_core" -eq "$base_core" ]] \
    || fail "deriveProfile must not grow corePackages (base $base_core, ext $ext_core)"
  [[ "$ext_pkgs" -eq $((base_pkgs + 1)) ]] \
    || fail "deriveProfile should append one package (base $base_pkgs, ext $ext_pkgs)"
  [[ "$ext_leaf" -eq 1 ]] || fail "extension leaf delta should be 1, got $ext_leaf"
}

# ============================================================================
test_pinned_toolchain_in_core() {
  local result
  result=$(eval_profile_json "
    let
      pinned = wlib.rustProfile {
        toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml;
        sha256 = \"$TOOLCHAIN_FIXTURE_SHA\";
        packages = [ np.hello ];
      };
    in {
      baseCore = coreLen wlib.profiles.base;
      pinnedCore = coreLen pinned;
      pinnedLeaf = leafLen pinned;
    }
  ")
  local base_core pinned_core pinned_leaf
  base_core=$(echo "$result" | jq -r '.baseCore')
  pinned_core=$(echo "$result" | jq -r '.pinnedCore')
  pinned_leaf=$(echo "$result" | jq -r '.pinnedLeaf')

  [[ "$pinned_core" -gt "$base_core" ]] \
    || fail "pinned toolchain should land in corePackages (base $base_core, pinned $pinned_core)"
  [[ "$pinned_leaf" -eq 2 ]] \
    || fail "only cargo-nextest and the extension package should be leaf, got $pinned_leaf"
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_base_core_equals_packages
  test_rust_toolchain_in_core
  test_python_extras_not_core
  test_extension_grows_packages_only
  test_pinned_toolchain_in_core
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
