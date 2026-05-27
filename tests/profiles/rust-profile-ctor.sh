#!/usr/bin/env bash
# Verify wrapix.rustProfile top-level constructor (specs/profiles.md).
#
#   test_required_args
#     wrapix.rustProfile {} and rustProfile { toolchain = ...; } (no sha256)
#     error at evaluation rather than silently producing an unpinned profile.
#
#   test_extension_args
#     rustProfile { toolchain; sha256; packages = [p]; env = { K = "v"; ... };
#                   mounts = [m]; networkAllowlist = [a]; }
#     lands extension args in the matching profile slots: packages, mounts,
#     and networkAllowlist appended after the pinned-profile base; env is
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
      pkgs = flake.legacyPackages.\"$system\";
      lib = pkgs.lib;
    in $expr
  "
}

# Assert that evaluating $1 fails (errors at evaluation). $2 is the label
# used in failure messages.
assert_eval_errors() {
  local expr="$1" label="$2"
  if eval_expr_raw "$expr" >/dev/null 2>/dev/null; then
    echo "FAIL: [$label] expected evaluation error, got success" >&2
    echo "  expr: $expr" >&2
    return 1
  fi
}

# ============================================================================
# rustProfile requires toolchain and sha256
# ============================================================================
test_required_args() {
  assert_eval_errors \
    "lib.rustProfile {}" \
    "empty args"

  assert_eval_errors \
    "lib.rustProfile { toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml; }" \
    "missing sha256"

  assert_eval_errors \
    "lib.rustProfile { sha256 = \"$TOOLCHAIN_FIXTURE_SHA\"; }" \
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
    env = { CTOR_TEST_KEY = \"ctor_test_value\"; SCCACHE_CACHE_SIZE = \"99G\"; };
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
      mountsDiff         = (builtins.length ext.mounts)   - (builtins.length base.mounts);
      networkDiff        = (builtins.length ext.networkAllowlist) - (builtins.length base.networkAllowlist);
      envExtAdded        = ext.env.CTOR_TEST_KEY or null;
      envRightMergeWins  = ext.env.SCCACHE_CACHE_SIZE;
      baseSccacheSize    = base.env.SCCACHE_CACHE_SIZE;
      mountDestPresent   = builtins.any (m: m.dest == \"/ctn/ctor\") ext.mounts;
      networkPresent     = builtins.elem \"ctor.example.com\" ext.networkAllowlist;
      baseNetworkPreserved = builtins.elem \"crates.io\" ext.networkAllowlist;
    }
  "); then
    echo "FAIL: nix eval rustProfile extension args expression failed" >&2
    return 1
  fi

  local packages_diff mounts_diff network_diff env_added env_override base_sccache \
    mount_present network_present base_net_preserved
  packages_diff=$(echo "$result"      | jq -r '.packagesDiff')
  mounts_diff=$(echo "$result"        | jq -r '.mountsDiff')
  network_diff=$(echo "$result"       | jq -r '.networkDiff')
  env_added=$(echo "$result"          | jq -r '.envExtAdded')
  env_override=$(echo "$result"       | jq -r '.envRightMergeWins')
  base_sccache=$(echo "$result"       | jq -r '.baseSccacheSize')
  mount_present=$(echo "$result"      | jq -r '.mountDestPresent')
  network_present=$(echo "$result"    | jq -r '.networkPresent')
  base_net_preserved=$(echo "$result" | jq -r '.baseNetworkPreserved')

  if [[ "$packages_diff" != "1" ]]; then
    echo "FAIL: packages should be appended (diff expected 1, got $packages_diff)" >&2
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
