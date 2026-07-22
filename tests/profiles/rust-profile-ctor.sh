#!/usr/bin/env bash
# Verify wrix.rustProfile top-level constructor (specs/profiles.md).
#
#   test_required_args
#     wrix.rustProfile {} and rustProfile { toolchain = ...; } (no sha256)
#     error at evaluation rather than silently producing an unpinned profile.
#
#   test_extension_args
#     rustProfile { toolchain; sha256; packages = [p]; hostPackages = [h];
#                   env = { K = "v"; ... }; mounts = [m]; networkAllowlist = [a]; }
#     lands extension args in the matching profile slots: packages, hostPackages,
#     mounts, and networkAllowlist appended after the pinned-profile base; env is
#     right-merged (consumer wins on conflict).
#
# Usage:
#   tests/profiles/rust-profile-ctor.sh                  # run both tests
#   tests/profiles/rust-profile-ctor.sh test_<name>      # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.75.0).
# Update this if the fixture's channel/components change.
TOOLCHAIN_FIXTURE_SHA="sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8="

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

# Evaluate a Nix expression against the live flake's lib. Stdout is the
# raw result; stderr is captured by the caller via redirection.
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

# Evaluate a Nix expression as JSON against the live flake's lib.
eval_expr_json() {
  local expr="$1"
  local system
  system=$(resolve_system)
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      pkgs = import flake.inputs.nixpkgs { system = \"$system\"; };
      lib = flake.legacyPackages.\"$system\".lib;
    in $expr
  "
}

assert_eval_error_contains() {
  local expr="$1"
  local expected="$2"
  local label="$3"
  local stderr_file
  stderr_file=$(mktemp)

  if eval_expr_raw "$expr" >/dev/null 2>"$stderr_file"; then
    printf 'FAIL: [%s] expected evaluation error, got success\n' "$label" >&2
    printf '  expr: %s\n' "$expr" >&2
    rm -f "$stderr_file"
    return 1
  fi
  if ! grep -F -- "$expected" "$stderr_file" >/dev/null; then
    printf 'FAIL: [%s] did not emit expected diagnostic: %s\n' "$label" "$expected" >&2
    cat "$stderr_file" >&2
    rm -f "$stderr_file"
    return 1
  fi
  rm -f "$stderr_file"
}

# ============================================================================
# rustProfile requires toolchain and sha256
# ============================================================================
test_required_args() {
  assert_eval_error_contains \
    "lib.rustProfile {}" \
    "function 'rustProfile' called without required argument 'toolchain'" \
    "empty args"

  assert_eval_error_contains \
    "lib.rustProfile { toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml; }" \
    "function 'rustProfile' called without required argument 'sha256'" \
    "missing sha256"

  assert_eval_error_contains \
    "lib.rustProfile { sha256 = \"$TOOLCHAIN_FIXTURE_SHA\"; }" \
    "function 'rustProfile' called without required argument 'toolchain'" \
    "missing toolchain"
}

# ============================================================================
# rustProfile extension args land in matching profile slots
# ============================================================================
test_extension_args() {
  local base_expr
  base_expr="lib.rustProfile {
    toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml;
    sha256 = \"$TOOLCHAIN_FIXTURE_SHA\";
  }"

  local ext_expr
  ext_expr="lib.rustProfile {
    toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml;
    sha256 = \"$TOOLCHAIN_FIXTURE_SHA\";
    packages = [ pkgs.hello ];
    hostPackages = [ pkgs.cowsay ];
    env = { CTOR_TEST_KEY = \"ctor_test_value\"; SCCACHE_CACHE_SIZE = \"99G\"; };
    runtimeSecrets = { CTOR_PROVIDER_TOKEN = \"required\"; };
    mounts = [ { source = \"/host/ctor\"; dest = \"/ctn/ctor\"; mode = \"ro\"; optional = true; } ];
    networkAllowlist = [ \"ctor.example.com\" ];
  }"

  local result
  if ! result=$(eval_expr_json "
    let
      base = $base_expr;
      ext  = $ext_expr;
    in {
      packagesDiff       = (builtins.length ext.packages) - (builtins.length base.packages);
      hostPackagesDiff   = (builtins.length ext.hostPackages) - (builtins.length base.hostPackages);
      mountsDiff         = (builtins.length ext.mounts)   - (builtins.length base.mounts);
      networkDiff        = (builtins.length ext.networkAllowlist) - (builtins.length base.networkAllowlist);
      envExtAdded        = ext.env.CTOR_TEST_KEY or null;
      envRightMergeWins  = ext.env.SCCACHE_CACHE_SIZE;
      baseSccacheSize    = base.env.SCCACHE_CACHE_SIZE;
      runtimeSecretPolicy = ext.runtimeSecrets.CTOR_PROVIDER_TOKEN or null;
      baseRuntimeSecretPolicy = ext.runtimeSecrets.OPENAI_API_KEY or null;
      hostPackagePresent = builtins.elem pkgs.cowsay.outPath (map (p: p.outPath) ext.hostPackages);
      hostPackageImageAbsent = ! (builtins.elem pkgs.cowsay.outPath (map (p: p.outPath) ext.packages));
      imagePackageHostAbsent = ! (builtins.elem pkgs.hello.outPath (map (p: p.outPath) ext.hostPackages));
      mountDestPresent   = builtins.any (m: m.dest == \"/ctn/ctor\") ext.mounts;
      networkPresent     = builtins.elem \"ctor.example.com\" ext.networkAllowlist;
      baseNetworkPreserved = builtins.elem \"crates.io\" ext.networkAllowlist;
    }
  "); then
    echo "FAIL: nix eval rustProfile extension args expression failed" >&2
    return 1
  fi

  local packages_diff host_packages_diff mounts_diff network_diff env_added env_override base_sccache \
    runtime_secret_policy base_runtime_secret_policy host_package_present \
    host_package_image_absent image_package_host_absent mount_present network_present \
    base_net_preserved
  packages_diff=$(echo "$result"             | jq -r '.packagesDiff')
  host_packages_diff=$(echo "$result"        | jq -r '.hostPackagesDiff')
  mounts_diff=$(echo "$result"               | jq -r '.mountsDiff')
  network_diff=$(echo "$result"              | jq -r '.networkDiff')
  env_added=$(echo "$result"                 | jq -r '.envExtAdded')
  env_override=$(echo "$result"              | jq -r '.envRightMergeWins')
  base_sccache=$(echo "$result"              | jq -r '.baseSccacheSize')
  runtime_secret_policy=$(echo "$result"     | jq -r '.runtimeSecretPolicy')
  base_runtime_secret_policy=$(echo "$result" | jq -r '.baseRuntimeSecretPolicy')
  host_package_present=$(echo "$result"      | jq -r '.hostPackagePresent')
  host_package_image_absent=$(echo "$result" | jq -r '.hostPackageImageAbsent')
  image_package_host_absent=$(echo "$result" | jq -r '.imagePackageHostAbsent')
  mount_present=$(echo "$result"             | jq -r '.mountDestPresent')
  network_present=$(echo "$result"           | jq -r '.networkPresent')
  base_net_preserved=$(echo "$result"        | jq -r '.baseNetworkPreserved')

  if [[ "$packages_diff" != "1" ]]; then
    echo "FAIL: packages should be appended (diff expected 1, got $packages_diff)" >&2
    return 1
  fi
  if [[ "$host_packages_diff" != "1" ]]; then
    echo "FAIL: hostPackages should be appended (diff expected 1, got $host_packages_diff)" >&2
    return 1
  fi
  if [[ "$mounts_diff" != "1" ]]; then
    echo "FAIL: mounts should be appended (diff expected 1, got $mounts_diff)" >&2
    return 1
  fi
  if [[ "$network_diff" != "1" ]]; then
    echo "FAIL: networkAllowlist should be appended (diff expected 1, got $network_diff)" >&2
    return 1
  fi
  if [[ "$env_added" != "ctor_test_value" ]]; then
    echo "FAIL: env.CTOR_TEST_KEY expected 'ctor_test_value', got '$env_added'" >&2
    return 1
  fi
  if [[ "$env_override" != "99G" ]]; then
    echo "FAIL: env.SCCACHE_CACHE_SIZE right-merge expected '99G', got '$env_override'" >&2
    return 1
  fi
  if [[ "$base_sccache" != "50G" ]]; then
    echo "FAIL: base env.SCCACHE_CACHE_SIZE expected '50G', got '$base_sccache' — base profile drift" >&2
    return 1
  fi
  if [[ "$runtime_secret_policy" != "required" ]]; then
    echo "FAIL: runtimeSecrets.CTOR_PROVIDER_TOKEN expected 'required', got '$runtime_secret_policy'" >&2
    return 1
  fi
  if [[ "$base_runtime_secret_policy" != "optional" ]]; then
    echo "FAIL: base OPENAI_API_KEY runtime-secret policy was not preserved" >&2
    return 1
  fi
  if [[ "$host_package_present" != "true" ]]; then
    echo "FAIL: hostPackages extension package missing from ext.hostPackages" >&2
    return 1
  fi
  if [[ "$host_package_image_absent" != "true" ]]; then
    echo "FAIL: hostPackages extension package should not be appended to image packages" >&2
    return 1
  fi
  if [[ "$image_package_host_absent" != "true" ]]; then
    echo "FAIL: image packages extension package should not be appended to hostPackages" >&2
    return 1
  fi
  if [[ "$mount_present" != "true" ]]; then
    echo "FAIL: extension mount dest /ctn/ctor missing from ext.mounts" >&2
    return 1
  fi
  if [[ "$network_present" != "true" ]]; then
    echo "FAIL: ctor.example.com missing from ext.networkAllowlist" >&2
    return 1
  fi
  if [[ "$base_net_preserved" != "true" ]]; then
    echo "FAIL: base networkAllowlist entry 'crates.io' was dropped — extension should append, not replace" >&2
    return 1
  fi
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_required_args
  test_extension_args
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
