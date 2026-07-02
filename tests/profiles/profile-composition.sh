#!/usr/bin/env bash
# Verify nested deriveProfile composition for profile attrsets.
#
# Usage:
#   tests/profiles/profile-composition.sh test_nested_derive_profile
#   tests/profiles/profile-composition.sh test_host_packages_split

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

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
      hostPackagesOf = prof: prof.hostPackages or [ ];
      hasPackage = needle: packages:
        builtins.any
          (p:
            let label = p.pname or (p.name or p.outPath);
            in lib.hasInfix needle label || lib.hasInfix needle p.outPath)
          packages;
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

test_nested_derive_profile() {
  local result
  result=$(eval_profile_json '
    let
      first = wlib.deriveProfile wlib.profiles.base {
        packages = [ np.hello ];
        env = {
          WRIX_COMPOSE_A = "first";
          WRIX_COMPOSE_B = "first";
        };
        mounts = [ { source = "/tmp/first"; dest = "/mnt/first"; mode = "ro"; optional = true; } ];
        networkAllowlist = [ "first.example" ];
      };
      second = wlib.deriveProfile first {
        packages = [ np.cowsay ];
        env = {
          WRIX_COMPOSE_B = "second";
          WRIX_COMPOSE_C = "second";
        };
        mounts = [ { source = "/tmp/second"; dest = "/mnt/second"; mode = "rw"; optional = true; } ];
        networkAllowlist = [ "second.example" ];
      };
    in {
      baseCore = builtins.length wlib.profiles.base.corePackages;
      secondCore = builtins.length second.corePackages;
      basePkgs = builtins.length wlib.profiles.base.packages;
      secondPkgs = builtins.length second.packages;
      secondLeaf = builtins.length (leaf second);
      mounts = map (m: m.dest) second.mounts;
      allowlist = second.networkAllowlist;
      envA = second.env.WRIX_COMPOSE_A;
      envB = second.env.WRIX_COMPOSE_B;
      envC = second.env.WRIX_COMPOSE_C;
      hasHelloLeaf = hasPackage "hello" (leaf second);
      hasCowsayLeaf = hasPackage "cowsay" (leaf second);
      hasHelloCore = hasPackage "hello" second.corePackages;
      hasCowsayCore = hasPackage "cowsay" second.corePackages;
    }
  ')

  local base_core second_core base_pkgs second_pkgs second_leaf
  base_core=$(json_field "$result" baseCore)
  second_core=$(json_field "$result" secondCore)
  base_pkgs=$(json_field "$result" basePkgs)
  second_pkgs=$(json_field "$result" secondPkgs)
  second_leaf=$(json_field "$result" secondLeaf)

  [[ "$second_core" -eq "$base_core" ]] || fail "nested deriveProfile changed corePackages (base $base_core, nested $second_core)"
  [[ "$second_pkgs" -eq $((base_pkgs + 2)) ]] || fail "nested deriveProfile should append two packages (base $base_pkgs, nested $second_pkgs)"
  [[ "$second_leaf" -eq 2 ]] || fail "nested leaf delta should contain the two extension packages, got $second_leaf"
  [[ "$(json_field "$result" envA)" == "first" ]] || fail "first-level env value was not preserved"
  [[ "$(json_field "$result" envB)" == "second" ]] || fail "second-level env did not right-override first-level env"
  [[ "$(json_field "$result" envC)" == "second" ]] || fail "second-level env value was not added"
  jq -e '.mounts == ["/mnt/first", "/mnt/second"]' <<<"$result" >/dev/null || fail "mounts were not concatenated in order"
  jq -e '.allowlist | index("first.example") and index("second.example")' <<<"$result" >/dev/null || fail "network allowlist did not include both extensions"
  [[ "$(json_field "$result" hasHelloLeaf)" == "true" ]] || fail "first extension package is missing from leaf delta"
  [[ "$(json_field "$result" hasCowsayLeaf)" == "true" ]] || fail "second extension package is missing from leaf delta"
  [[ "$(json_field "$result" hasHelloCore)" == "false" ]] || fail "first extension package leaked into corePackages"
  [[ "$(json_field "$result" hasCowsayCore)" == "false" ]] || fail "second extension package leaked into corePackages"
}

test_host_packages_split() {
  local result
  result=$(eval_profile_json '
    let
      base = wlib.profiles.base;
      ext = wlib.deriveProfile base {
        packages = [ np.hello ];
        hostPackages = [ np.cowsay ];
      };
    in {
      packagesDiff = (builtins.length ext.packages) - (builtins.length base.packages);
      hostPackagesDiff = (builtins.length (hostPackagesOf ext)) - (builtins.length (hostPackagesOf base));
      hasImageHello = hasPackage "hello" ext.packages;
      hasHostCowsay = hasPackage "cowsay" (hostPackagesOf ext);
      hasHostHello = hasPackage "hello" (hostPackagesOf ext);
      hasImageCowsay = hasPackage "cowsay" ext.packages;
    }
  ')

  [[ "$(json_field "$result" packagesDiff)" -eq 1 ]] || fail "packages extension should append exactly one image package"
  [[ "$(json_field "$result" hostPackagesDiff)" -eq 1 ]] || fail "hostPackages extension should append exactly one host package"
  [[ "$(json_field "$result" hasImageHello)" == "true" ]] || fail "image package extension missing from packages"
  [[ "$(json_field "$result" hasHostCowsay)" == "true" ]] || fail "hostPackages extension missing from hostPackages"
  [[ "$(json_field "$result" hasHostHello)" == "false" ]] || fail "image package extension leaked into hostPackages"
  [[ "$(json_field "$result" hasImageCowsay)" == "false" ]] || fail "hostPackages extension leaked into packages"
}

ALL_TESTS=(
  test_nested_derive_profile
  test_host_packages_split
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
