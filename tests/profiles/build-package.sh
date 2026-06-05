#!/usr/bin/env bash
# Verify the seven [verify] hash invariants of profiles.rust.buildPackage:
#
#   1. test_build_package_exposed
#      buildPackage exists and returns { bin, clippy, nextest, cargoArtifacts }.
#   2. test_workspace_edit_reuses_dep_cache
#      Editing a workspace .rs invalidates bin but not cargoArtifacts.
#   3. test_source_filter_excludes_non_cargo
#      Adding/editing a *.md inside src does not change bin/clippy/nextest.
#   4. test_workspace_edit_skips_cargo_artifacts
#      Editing a workspace .rs invalidates bin+clippy+nextest but not cargoArtifacts.
#   5. test_extra_srcs_scoped_to_lint_test
#      Editing a file in extraSrcs invalidates clippy+nextest only.
#   6. test_build_package_toolchain_alignment
#      bin/clippy/nextest all close over profile.toolchain on both
#      profiles.rust and rustProfile { toolchain; sha256; }.
#   7. test_consumers_migrated
#      lib/mcp/tmux/mcp-server.nix consumes profile.buildPackage (no
#      rustPlatform.buildRustPackage / makeRustPlatform); tests/default.nix
#      exposes the clippy/nextest check entries; modules/flake/packages.nix
#      extracts .bin from tmuxMcpPackage.
#
# Usage:
#   tests/profiles/build-package.sh                    # run all 7 tests
#   tests/profiles/build-package.sh test_<name>        # run a single test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/build-package-fixture"

# Pinned sha256 for tests/fixtures/rust-toolchain.toml (channel 1.75.0).
# Same value as no-nightly-closure.sh; update both if the fixture changes.
TOOLCHAIN_FIXTURE_SHA="sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8="

TMPDIRS=()
cleanup() {
  for d in "${TMPDIRS[@]+"${TMPDIRS[@]}"}"; do
    [[ -n "$d" ]] && [[ -d "$d" ]] && rm -rf "$d"
  done
}
trap cleanup EXIT

make_fixture() {
  local dst
  dst=$(mktemp -d -t build-package-fixture.XXXXXX)
  TMPDIRS+=("$dst")
  cp -r "$FIXTURE_DIR/." "$dst/"
  chmod -R u+w "$dst"
  echo "$dst"
}

make_extras_dir() {
  local dst
  dst=$(mktemp -d -t build-package-extras.XXXXXX)
  TMPDIRS+=("$dst")
  echo "$dst"
}

# Eval drvPaths for buildPackage applied to the given fixture path.
# Args:
#   $1 fixture_path (absolute)
#   $2 extras_expr  (Nix attrset expression, e.g. '{}' or '{ "data.txt" = /tmp/x; }')
#   $3 profile_expr (Nix expression returning the rust profile attrset)
# Stdout: JSON { bin, clippy, nextest, cargoArtifacts, toolchain }
eval_drvs() {
  local fixture="$1"
  local extras_expr="$2"
  local profile_expr="$3"

  nix eval --json --impure --no-warn-dirty --expr "
    let
      system = builtins.currentSystem;
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\${system}.lib;
      profile = $profile_expr;
      pkg = profile.buildPackage {
        src = $fixture;
        cargoLock = $fixture/Cargo.lock;
        extraSrcs = $extras_expr;
      };
    in {
      bin = pkg.bin.drvPath;
      clippy = pkg.clippy.drvPath;
      nextest = pkg.nextest.drvPath;
      cargoArtifacts = pkg.cargoArtifacts.drvPath;
      toolchain = profile.toolchain.drvPath;
    }
  "
}

assert_eq() {
  local label="$1" got="$2" want="$3"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL: $label: expected equal" >&2
    echo "  got:  $got" >&2
    echo "  want: $want" >&2
    return 1
  fi
}

assert_ne() {
  local label="$1" got="$2" other="$3"
  if [[ "$got" = "$other" ]]; then
    echo "FAIL: $label: expected different drvPaths, both were $got" >&2
    return 1
  fi
}

# ============================================================================
# 1. buildPackage exposed and returns the documented attrset
# ============================================================================
test_build_package_exposed() {
  local fixture; fixture=$(make_fixture)

  local result
  if ! result=$(nix eval --json --impure --no-warn-dirty --expr "
    let
      system = builtins.currentSystem;
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\${system}.lib;
      profile = lib.profiles.rust;
      pkg = profile.buildPackage {
        src = $fixture;
        cargoLock = $fixture/Cargo.lock;
      };
    in {
      hasBuildPackage = builtins.isFunction profile.buildPackage;
      keys = builtins.attrNames pkg;
      types = builtins.mapAttrs (_: v: v.type or null) pkg;
    }
  "); then
    echo "FAIL: nix eval profiles.rust.buildPackage failed" >&2
    return 1
  fi

  local hasFn
  hasFn=$(echo "$result" | jq -r '.hasBuildPackage')
  if [[ "$hasFn" != "true" ]]; then
    echo "FAIL: profiles.rust.buildPackage is not a function" >&2
    return 1
  fi

  local keys
  keys=$(echo "$result" | jq -r '.keys | sort | join(",")')
  local want_keys="bin,cargoArtifacts,clippy,nextest"
  if [[ "$keys" != "$want_keys" ]]; then
    echo "FAIL: buildPackage returned keys [$keys], expected [$want_keys]" >&2
    return 1
  fi

  local bad
  bad=$(echo "$result" | jq -r '.types | to_entries | map(select(.value != "derivation")) | map(.key) | join(",")')
  if [[ -n "$bad" ]]; then
    echo "FAIL: buildPackage outputs that are not derivations: $bad" >&2
    return 1
  fi
}

# ============================================================================
# 2. Editing a workspace .rs changes bin but not cargoArtifacts
# ============================================================================
test_workspace_edit_reuses_dep_cache() {
  local fixture; fixture=$(make_fixture)
  local profile='lib.profiles.rust'

  local before after
  before=$(eval_drvs "$fixture" '{}' "$profile")
  echo "" >> "$fixture/src/main.rs"
  echo "// edited for hash test" >> "$fixture/src/main.rs"
  after=$(eval_drvs "$fixture" '{}' "$profile")

  local bin_before bin_after ca_before ca_after
  bin_before=$(echo "$before" | jq -r '.bin')
  bin_after=$(echo "$after"  | jq -r '.bin')
  ca_before=$(echo "$before" | jq -r '.cargoArtifacts')
  ca_after=$(echo "$after"  | jq -r '.cargoArtifacts')

  assert_ne "bin should change after .rs edit" "$bin_after" "$bin_before"
  assert_eq "cargoArtifacts should be reused after .rs edit" "$ca_after" "$ca_before"
}

# ============================================================================
# 3. Adding/editing a *.md in src is filtered out
# ============================================================================
test_source_filter_excludes_non_cargo() {
  local fixture; fixture=$(make_fixture)
  local profile='lib.profiles.rust'

  local before after
  before=$(eval_drvs "$fixture" '{}' "$profile")
  printf 'doc v1\n' > "$fixture/src/NOTES.md"
  printf 'top-level doc v1\n' > "$fixture/README.md"
  after=$(eval_drvs "$fixture" '{}' "$profile")

  local key
  for key in bin clippy nextest cargoArtifacts; do
    local b a
    b=$(echo "$before" | jq -r ".$key")
    a=$(echo "$after"  | jq -r ".$key")
    assert_eq "$key unchanged after adding *.md" "$a" "$b"
  done
}

# ============================================================================
# 4. Editing a workspace .rs invalidates bin+clippy+nextest but not cargoArtifacts
# ============================================================================
test_workspace_edit_skips_cargo_artifacts() {
  local fixture; fixture=$(make_fixture)
  local profile='lib.profiles.rust'

  local before after
  before=$(eval_drvs "$fixture" '{}' "$profile")
  printf '\n// rs touched\n' >> "$fixture/src/main.rs"
  after=$(eval_drvs "$fixture" '{}' "$profile")

  local key
  for key in bin clippy nextest; do
    local b a
    b=$(echo "$before" | jq -r ".$key")
    a=$(echo "$after"  | jq -r ".$key")
    assert_ne "$key should change after .rs edit" "$a" "$b"
  done

  local ca_before ca_after
  ca_before=$(echo "$before" | jq -r '.cargoArtifacts')
  ca_after=$(echo "$after"  | jq -r '.cargoArtifacts')
  assert_eq "cargoArtifacts unchanged after .rs edit" "$ca_after" "$ca_before"
}

# ============================================================================
# 5. extraSrcs edits invalidate clippy+nextest but not bin or cargoArtifacts
# ============================================================================
test_extra_srcs_scoped_to_lint_test() {
  local fixture; fixture=$(make_fixture)
  local extras_dir; extras_dir=$(make_extras_dir)
  local profile='lib.profiles.rust'

  local extras_file="$extras_dir/data.txt"
  printf 'extras v1\n' > "$extras_file"
  local extras_expr="{ \"extras/data.txt\" = $extras_file; }"

  local before after
  before=$(eval_drvs "$fixture" "$extras_expr" "$profile")
  printf 'extras v2\n' > "$extras_file"
  after=$(eval_drvs "$fixture" "$extras_expr" "$profile")

  local key
  for key in clippy nextest; do
    local b a
    b=$(echo "$before" | jq -r ".$key")
    a=$(echo "$after"  | jq -r ".$key")
    assert_ne "$key should change after extraSrcs edit" "$a" "$b"
  done

  local key2
  for key2 in bin cargoArtifacts; do
    local b a
    b=$(echo "$before" | jq -r ".$key2")
    a=$(echo "$after"  | jq -r ".$key2")
    assert_eq "$key2 unchanged after extraSrcs edit" "$a" "$b"
  done
}

# ============================================================================
# 6. bin/clippy/nextest all close over profile.toolchain
# ============================================================================
test_build_package_toolchain_alignment() {
  local fixture; fixture=$(make_fixture)

  local default_profile='lib.profiles.rust'
  local with_profile="lib.rustProfile { toolchain = $REPO_ROOT/tests/fixtures/rust-toolchain.toml; sha256 = \"$TOOLCHAIN_FIXTURE_SHA\"; }"

  local label
  for label in "default:$default_profile" "rustProfile:$with_profile"; do
    local name="${label%%:*}"
    local profile_expr="${label#*:}"

    local result
    result=$(eval_drvs "$fixture" '{}' "$profile_expr")

    local toolchain bin clippy nextest
    toolchain=$(echo "$result" | jq -r '.toolchain')
    bin=$(echo "$result" | jq -r '.bin')
    clippy=$(echo "$result" | jq -r '.clippy')
    nextest=$(echo "$result" | jq -r '.nextest')

    if [[ -z "$toolchain" ]] || [[ "$toolchain" = "null" ]]; then
      echo "FAIL: [$name] profile.toolchain.drvPath is empty" >&2
      return 1
    fi

    local drv
    for drv in "$bin" "$clippy" "$nextest"; do
      if ! nix-store -q --requisites "$drv" | grep -qxF "$toolchain"; then
        echo "FAIL: [$name] $drv does not reference toolchain $toolchain" >&2
        return 1
      fi
    done
  done
}

# ============================================================================
# 7. Consumers migrated to profile.buildPackage and wired through to checks
# ============================================================================
test_consumers_migrated() {
  local file pat
  file="$REPO_ROOT/lib/mcp/tmux/mcp-server.nix"
  if [[ ! -f "$file" ]]; then
    echo "FAIL: expected consumer file missing: $file" >&2
    return 1
  fi
  for pat in 'rustPlatform.buildRustPackage' 'makeRustPlatform'; do
    if grep -qF "$pat" "$file"; then
      echo "FAIL: $file still references '$pat'" >&2
      return 1
    fi
  done
  if ! grep -qF 'rustProfile.buildPackage' "$file"; then
    echo "FAIL: $file does not call rustProfile.buildPackage" >&2
    return 1
  fi

  local checks_file="$REPO_ROOT/tests/default.nix"
  for pat in tmux-mcp-clippy tmux-mcp-nextest; do
    if ! grep -qF "$pat" "$checks_file"; then
      echo "FAIL: $checks_file is missing check entry '$pat'" >&2
      return 1
    fi
  done

  local pkgs_file="$REPO_ROOT/modules/flake/packages.nix"
  if ! grep -qF 'wrix.tmuxMcpPackage.bin' "$pkgs_file"; then
    echo "FAIL: $pkgs_file does not extract .bin via 'wrix.tmuxMcpPackage.bin'" >&2
    return 1
  fi
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_build_package_exposed
  test_workspace_edit_reuses_dep_cache
  test_source_filter_excludes_non_cargo
  test_workspace_edit_skips_cargo_artifacts
  test_extra_srcs_scoped_to_lint_test
  test_build_package_toolchain_alignment
  test_consumers_migrated
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
