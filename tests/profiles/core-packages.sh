#!/usr/bin/env bash
# Verify the corePackages tier-1 membership key on profile attrsets.
#
# corePackages is the wrix-controlled, fixed-per-instance package set.
# Downstream extension grows `packages` only, never `corePackages`, so the
# leaf delta an image rebuilds on is `packages` − `corePackages`.
#
#   test_core_membership
#     Built-in profiles keep base packages in corePackages. The base floor
#     includes shared scripting/build tools; Rust and Python fixed extras stay
#     in corePackages. cargo-nextest remains Rust leaf tooling, and pinned
#     rustProfile toolchains also land in core.
#
#   test_base_python_boundary
#     Base exposes python3 on image and host surfaces, while uv/ruff/ty,
#     UV_CACHE_DIR, and the uv cache mount remain scoped to the Python profile.
#
#   test_extra_not_in_core
#     deriveProfile appends extension packages to packages without changing
#     corePackages, so the added package appears only in the leaf delta.
#
# Usage:
#   tests/profiles/core-packages.sh                 # run all tests
#   tests/profiles/core-packages.sh test_<name>     # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.75.0).
TOOLCHAIN_FIXTURE_SHA="sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8="

require_tools() {
  local tool
  for tool in nix jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "SKIP: $tool is required" >&2
      exit 77
    fi
  done
}

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

# Evaluate $1 as JSON against the live flake. The expression may reference
# `wlib` (wrix lib), `np` (nixpkgs package set), `lib` (nixpkgs lib), plus
# helper functions for core, package, and leaf membership.
eval_profile_json() {
  local expr="$1"
  local system
  require_tools
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
      packageLabel = p: p.pname or (p.name or (builtins.baseNameOf p.outPath));
      packageMatches = needle: p: lib.hasInfix needle (packageLabel p);
      countPackage = needle: packages: builtins.length (builtins.filter (packageMatches needle) packages);
      hasPackage = needle: packages: countPackage needle packages > 0;
    in $expr
  "
}

fail() {
  echo "FAIL: $1" >&2
  return 1
}

json_field() {
  local json="$1"
  local field="$2"
  jq -r ".$field" <<<"$json"
}

# ============================================================================
test_core_membership() {
  local result
  result=$(eval_profile_json "
    let
      base = wlib.profiles.base;
      rust = wlib.profiles.rust;
      python = wlib.profiles.python;
      pinned = wlib.rustProfile {
        toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml;
        sha256 = \"$TOOLCHAIN_FIXTURE_SHA\";
        packages = [ np.hello ];
      };
    in {
      baseCore = coreLen base;
      basePkgs = pkgsLen base;
      baseLeaf = leafLen base;
      rustCore = coreLen rust;
      rustLeaf = leafLen rust;
      pythonCore = coreLen python;
      pythonLeaf = leafLen python;
      pinnedCore = coreLen pinned;
      pinnedLeaf = leafLen pinned;
      hasRustSccacheCore = hasPackage \"sccache\" rust.corePackages;
      hasRustNextestLeaf = hasPackage \"cargo-nextest\" (leaf rust);
      hasRustNextestCore = hasPackage \"cargo-nextest\" rust.corePackages;
      hasMakeCore = hasPackage \"gnumake\" base.corePackages || hasPackage \"make\" base.corePackages;
      hasOpenSshCore = hasPackage \"openssh\" base.corePackages;
      hasBasePythonCore = hasPackage \"python3\" base.corePackages;
      hasPythonCore = hasPackage \"python3\" python.corePackages;
      hasUvCore = hasPackage \"uv\" python.corePackages;
      hasRuffCore = hasPackage \"ruff\" python.corePackages;
      hasTyCore = hasPackage \"ty\" python.corePackages;
      hasUvLeaf = hasPackage \"uv\" (leaf python);
      hasPinnedRustCore = hasPackage \"rust\" pinned.corePackages;
      hasPinnedHelloLeaf = hasPackage \"hello\" (leaf pinned);
    }
  ")

  local base_core base_pkgs base_leaf rust_core rust_leaf python_core python_leaf pinned_core pinned_leaf
  base_core=$(json_field "$result" baseCore)
  base_pkgs=$(json_field "$result" basePkgs)
  base_leaf=$(json_field "$result" baseLeaf)
  rust_core=$(json_field "$result" rustCore)
  rust_leaf=$(json_field "$result" rustLeaf)
  python_core=$(json_field "$result" pythonCore)
  python_leaf=$(json_field "$result" pythonLeaf)
  pinned_core=$(json_field "$result" pinnedCore)
  pinned_leaf=$(json_field "$result" pinnedLeaf)

  [[ "$base_core" -gt 0 ]] || fail "base corePackages should be non-empty, got $base_core"
  [[ "$base_core" -eq "$base_pkgs" ]] || fail "base corePackages ($base_core) should equal packages ($base_pkgs)"
  [[ "$base_leaf" -eq 0 ]] || fail "base leaf delta should be empty, got $base_leaf"
  [[ "$(json_field "$result" hasMakeCore)" == "true" ]] || fail "make should be a member of base corePackages"
  [[ "$(json_field "$result" hasOpenSshCore)" == "true" ]] || fail "openssh should be a member of base corePackages"
  [[ "$(json_field "$result" hasBasePythonCore)" == "true" ]] || fail "python3 should be a member of base corePackages"
  [[ "$rust_core" -gt "$base_core" ]] || fail "rust corePackages ($rust_core) should exceed base ($base_core)"
  [[ "$rust_leaf" -eq 1 ]] || fail "rust leaf delta should contain only cargo-nextest, got $rust_leaf"
  [[ "$python_core" -gt "$base_core" ]] || fail "python corePackages ($python_core) should exceed base ($base_core)"
  [[ "$python_leaf" -eq 0 ]] || fail "python leaf delta should be empty, got $python_leaf"
  [[ "$pinned_core" -gt "$base_core" ]] || fail "pinned rust corePackages ($pinned_core) should exceed base ($base_core)"
  [[ "$pinned_leaf" -eq 2 ]] || fail "pinned leaf should contain cargo-nextest and hello, got $pinned_leaf"

  [[ "$(json_field "$result" hasRustSccacheCore)" == "true" ]] || fail "sccache should be a member of rust corePackages"
  [[ "$(json_field "$result" hasRustNextestLeaf)" == "true" ]] || fail "cargo-nextest should be rust leaf tooling"
  [[ "$(json_field "$result" hasRustNextestCore)" == "false" ]] || fail "cargo-nextest should not be a member of rust corePackages"
  [[ "$(json_field "$result" hasPythonCore)" == "true" ]] || fail "python3 should be a member of python corePackages"
  [[ "$(json_field "$result" hasUvCore)" == "true" ]] || fail "uv should be a member of python corePackages"
  [[ "$(json_field "$result" hasRuffCore)" == "true" ]] || fail "ruff should be a member of python corePackages"
  [[ "$(json_field "$result" hasTyCore)" == "true" ]] || fail "ty should be a member of python corePackages"
  [[ "$(json_field "$result" hasUvLeaf)" == "false" ]] || fail "uv should not be python leaf tooling"
  [[ "$(json_field "$result" hasPinnedRustCore)" == "true" ]] || fail "pinned rust toolchain should be a member of corePackages"
  [[ "$(json_field "$result" hasPinnedHelloLeaf)" == "true" ]] || fail "extension package should remain leaf on pinned rustProfile"
}

# ============================================================================
test_base_python_boundary() {
  local result
  result=$(eval_profile_json '
    let
      base = wlib.profiles.base;
      rust = wlib.profiles.rust;
      python = wlib.profiles.python;
      uvMounts = prof: builtins.filter (m: (m.dest or "") == "/home/wrix/.cache/uv") (prof.mounts or [ ]);
      hasUvCacheEnv = prof: prof.env ? UV_CACHE_DIR;
    in {
      baseImagePython = countPackage "python3" base.packages;
      baseCorePython = countPackage "python3" base.corePackages;
      baseHostPython = countPackage "python3" base.hostPackages;
      pythonImagePython = countPackage "python3" python.packages;
      pythonCorePython = countPackage "python3" python.corePackages;
      pythonHostPython = countPackage "python3" python.hostPackages;
      baseImageUv = hasPackage "uv" base.packages;
      baseHostUv = hasPackage "uv" base.hostPackages;
      baseImageRuff = hasPackage "ruff" base.packages;
      baseHostRuff = hasPackage "ruff" base.hostPackages;
      baseImageTy = hasPackage "ty" base.packages;
      baseHostTy = hasPackage "ty" base.hostPackages;
      rustImageUv = hasPackage "uv" rust.packages;
      rustHostUv = hasPackage "uv" rust.hostPackages;
      rustImageRuff = hasPackage "ruff" rust.packages;
      rustHostRuff = hasPackage "ruff" rust.hostPackages;
      rustImageTy = hasPackage "ty" rust.packages;
      rustHostTy = hasPackage "ty" rust.hostPackages;
      pythonImageUv = hasPackage "uv" python.packages;
      pythonHostUv = hasPackage "uv" python.hostPackages;
      pythonImageRuff = hasPackage "ruff" python.packages;
      pythonHostRuff = hasPackage "ruff" python.hostPackages;
      pythonImageTy = hasPackage "ty" python.packages;
      pythonHostTy = hasPackage "ty" python.hostPackages;
      baseEnvUvCache = hasUvCacheEnv base;
      rustEnvUvCache = hasUvCacheEnv rust;
      pythonEnvUvCache = python.env.UV_CACHE_DIR or "";
      baseUvMountCount = builtins.length (uvMounts base);
      rustUvMountCount = builtins.length (uvMounts rust);
      pythonUvMountCount = builtins.length (uvMounts python);
      pythonUvMountWritable =
        builtins.length (uvMounts python) == 1
        && builtins.all (m: (m.mode or "") == "rw" && (m.optional or false)) (uvMounts python);
    }
  ')

  local base_image_python base_core_python base_host_python
  local python_image_python python_core_python python_host_python
  base_image_python=$(json_field "$result" baseImagePython)
  base_core_python=$(json_field "$result" baseCorePython)
  base_host_python=$(json_field "$result" baseHostPython)
  python_image_python=$(json_field "$result" pythonImagePython)
  python_core_python=$(json_field "$result" pythonCorePython)
  python_host_python=$(json_field "$result" pythonHostPython)

  [[ "$base_image_python" -gt 0 ]] || fail "base packages should expose python3"
  [[ "$base_core_python" -gt 0 ]] || fail "base corePackages should expose python3"
  [[ "$base_host_python" -gt 0 ]] || fail "base hostPackages should expose python3"
  [[ "$python_image_python" -eq "$base_image_python" ]] || fail "python profile packages should inherit python3 without adding a duplicate"
  [[ "$python_core_python" -eq "$base_core_python" ]] || fail "python profile corePackages should inherit python3 without adding a duplicate"
  [[ "$python_host_python" -eq "$base_host_python" ]] || fail "python profile hostPackages should inherit python3 without adding a duplicate"

  jq -e '
    (.baseImageUv | not) and (.baseHostUv | not) and
    (.baseImageRuff | not) and (.baseHostRuff | not) and
    (.baseImageTy | not) and (.baseHostTy | not) and
    (.rustImageUv | not) and (.rustHostUv | not) and
    (.rustImageRuff | not) and (.rustHostRuff | not) and
    (.rustImageTy | not) and (.rustHostTy | not)
  ' <<<"$result" >/dev/null || fail "uv, ruff, and ty should be scoped to the Python profile"
  jq -e '
    .pythonImageUv and .pythonHostUv and
    .pythonImageRuff and .pythonHostRuff and
    .pythonImageTy and .pythonHostTy
  ' <<<"$result" >/dev/null || fail "python profile should expose uv, ruff, and ty on image and host surfaces"

  [[ "$(json_field "$result" baseEnvUvCache)" == "false" ]] || fail "base env should not set UV_CACHE_DIR"
  [[ "$(json_field "$result" rustEnvUvCache)" == "false" ]] || fail "rust env should not set UV_CACHE_DIR"
  [[ "$(json_field "$result" pythonEnvUvCache)" == "/home/wrix/.cache/uv" ]] || fail "python env should set UV_CACHE_DIR to the uv cache path"
  [[ "$(json_field "$result" baseUvMountCount)" -eq 0 ]] || fail "base should not mount the uv cache"
  [[ "$(json_field "$result" rustUvMountCount)" -eq 0 ]] || fail "rust should not mount the uv cache"
  [[ "$(json_field "$result" pythonUvMountCount)" -eq 1 ]] || fail "python should mount the uv cache exactly once"
  [[ "$(json_field "$result" pythonUvMountWritable)" == "true" ]] || fail "python uv cache mount should be writable and optional"
}

# ============================================================================
test_extra_not_in_core() {
  local result
  result=$(eval_profile_json "
    let
      base = wlib.profiles.base;
      ext = wlib.deriveProfile base { packages = [ np.hello ]; };
    in {
      baseCore = coreLen base;
      basePkgs = pkgsLen base;
      baseLeaf = leafLen base;
      extCore = coreLen ext;
      extPkgs = pkgsLen ext;
      extLeaf = leafLen ext;
      hasHelloLeaf = hasPackage \"hello\" (leaf ext);
      hasHelloCore = hasPackage \"hello\" ext.corePackages;
    }
  ")

  local base_core base_pkgs base_leaf ext_core ext_pkgs ext_leaf
  base_core=$(json_field "$result" baseCore)
  base_pkgs=$(json_field "$result" basePkgs)
  base_leaf=$(json_field "$result" baseLeaf)
  ext_core=$(json_field "$result" extCore)
  ext_pkgs=$(json_field "$result" extPkgs)
  ext_leaf=$(json_field "$result" extLeaf)

  [[ "$ext_core" -eq "$base_core" ]] || fail "deriveProfile must not grow corePackages (base $base_core, ext $ext_core)"
  [[ "$ext_pkgs" -eq $((base_pkgs + 1)) ]] || fail "deriveProfile should append one package (base $base_pkgs, ext $ext_pkgs)"
  [[ "$ext_leaf" -eq $((base_leaf + 1)) ]] || fail "extension leaf delta should grow by 1 (base $base_leaf, ext $ext_leaf)"
  [[ "$(json_field "$result" hasHelloLeaf)" == "true" ]] || fail "extension package should be in packages − corePackages"
  [[ "$(json_field "$result" hasHelloCore)" == "false" ]] || fail "extension package should not be in corePackages"
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_core_membership
  test_base_python_boundary
  test_extra_not_in_core
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
