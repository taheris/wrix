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

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.85.1).
TOOLCHAIN_FIXTURE_SHA="sha256-Hn2uaQzRLidAWpfmRwSRdImifGUCAb9HeAqTYFXWeQk="

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
      imageSystem =
        if system == \"aarch64-darwin\" then
          \"aarch64-linux\"
        else if system == \"x86_64-darwin\" then
          \"x86_64-linux\"
        else
          system;
      imagePkgs = flake.inputs.nixpkgs.legacyPackages.\${imageSystem};
      packageLabel = package:
        builtins.unsafeDiscardStringContext
          (package.pname or (package.name or (builtins.baseNameOf package.outPath)));
      pathOf = package: builtins.unsafeDiscardStringContext package.outPath;
      sortPaths = packages: builtins.sort builtins.lessThan (map pathOf packages);
      sortPathStrings = builtins.sort builtins.lessThan;
      expectedBaseDirectPackages = with imagePkgs; [
        bash
        beads
        coreutils
        curl
        diffutils
        dolt
        fd
        file
        findutils
        gawk
        gh
        git
        gnugrep
        gnused
        gnutar
        gnumake
        gzip
        jq
        less
        lsof
        man
        nix
        openssh
        patch
        prek
        python3
        ripgrep
        rsync
        shellcheck
        sqlite
        tmux
        tree
        unzip
        vim
        yq
        zip
        getent.provider
        iproute2
        nftables
        iptables
        iputils
        libcap
        netcat
        procps
        util-linux
      ];
      expectedBaseDirect = sortPaths expectedBaseDirectPackages;
      baseExtras = builtins.filter (
        package: !(builtins.elem (pathOf package) expectedBaseDirect)
      ) base.corePackages;
      expectedBase = sortPathStrings (expectedBaseDirect ++ map pathOf baseExtras);
      rustSupport = with imagePkgs; [
        gcc
        openssl
        openssl.dev
        pkg-config
        postgresql.lib
        sccache
      ];
      imageToolchain = profile:
        builtins.unsafeDiscardStringContext (builtins.dirOf (builtins.dirOf profile.env.RUSTC));
      expectedRustCoreFor = profile:
        sortPathStrings (expectedBase ++ [ (imageToolchain profile) ] ++ sortPaths rustSupport);
      expectedRustCore = expectedRustCoreFor rust;
      expectedPinnedCore = expectedRustCoreFor pinned;
      expectedPythonCore = sortPathStrings (expectedBase ++ sortPaths (with imagePkgs; [ ruff ty uv ]));
      expectedRustPackages = sortPathStrings (expectedRustCore ++ sortPaths [ imagePkgs.cargo-nextest ]);
      expectedPinnedPackages = sortPathStrings (
        expectedPinnedCore ++ sortPaths [ imagePkgs.cargo-nextest np.hello ]
      );
      countPath = path: packages:
        builtins.length (builtins.filter (package: package.outPath == path) packages);
    in {
      inherit expectedBase expectedRustCore expectedPinnedCore expectedPythonCore expectedRustPackages expectedPinnedPackages;
      baseExtras = builtins.sort builtins.lessThan (map packageLabel baseExtras);
      baseCore = sortPaths base.corePackages;
      basePackages = sortPaths base.packages;
      rustCore = sortPaths rust.corePackages;
      rustPackages = sortPaths rust.packages;
      pythonCore = sortPaths python.corePackages;
      pythonPackages = sortPaths python.packages;
      pinnedCore = sortPaths pinned.corePackages;
      pinnedPackages = sortPaths pinned.packages;
      rustImageToolchainCoreCount = countPath (imageToolchain rust) rust.corePackages;
      pinnedImageToolchainCoreCount = countPath (imageToolchain pinned) pinned.corePackages;
    }
  ")

  jq -e '
    .baseExtras == ["treefmt", "which"] and
    .baseCore == .expectedBase and
    .basePackages == .expectedBase and
    .rustCore == .expectedRustCore and
    .rustPackages == .expectedRustPackages and
    .pythonCore == .expectedPythonCore and
    .pythonPackages == .expectedPythonCore and
    .pinnedCore == .expectedPinnedCore and
    .pinnedPackages == .expectedPinnedPackages and
    .rustImageToolchainCoreCount == 1 and
    .pinnedImageToolchainCoreCount == 1
  ' <<<"$result" >/dev/null || fail "profile package membership differs from the documented exact sets"
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
