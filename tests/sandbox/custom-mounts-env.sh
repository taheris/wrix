#!/usr/bin/env bash
# Verifier for criterion 110 of specs/sandbox.md:
#
#   `mounts` and `env` passed to `mkSandbox` are merged into the profile
#   and reach the container as configured.
#
# The merge contract lives in `extendProfile` (lib/sandbox/default.nix):
# extra mounts are appended to `profile.mounts`, extra env is right-merged
# into `profile.env` (consumer wins on key collision). The launcher
# consumes `profile.mounts` via `mkMountSpecs` (`-v src:dest:mode` for
# podman / VirtioFS args for Apple `container`); the image bakes
# `profile.env` into the container's `Env` directive
# (lib/sandbox/image.nix). Both touch the same resolved profile that
# `mkSandbox` returns as its `profile` field, so the resolved profile is
# honest evidence of what reaches the container.
#
# We drive `nix eval` against the flake for merge invariants, then invoke the
# live Rust launcher with mocked podman/skopeo so the resolved env and mounts
# are observed at the container argv boundary rather than inferred from source.
#
# Stylistic template: tests/profiles/mkdevshell.sh:test_env_right_merge.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

eval_expr_json() {
  local expr="$1"
  local system
  system=$(resolve_system)
  nix eval --json --impure --no-warn-dirty --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\"$system\".lib;
    in $expr
  "
}

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

# ============================================================================
# Extra mounts append to profile.mounts (extension, not replacement).
#
# Uses profiles.python which ships with a non-empty mounts list (UV cache).
# Adding two consumer mounts must grow mounts by exactly two; the existing
# UV cache mount must still be present.
# ============================================================================
test_mounts_appended_to_profile() {
  local result
  if ! result=$(eval_expr_json "
    let
      baseline = lib.mkSandbox { profile = lib.profiles.python; };
      extended = lib.mkSandbox {
        profile = lib.profiles.python;
        mounts = [
          { source = \"/tmp/wrix-test-src-a\"; dest = \"/mnt/a\"; mode = \"ro\"; }
          { source = \"/tmp/wrix-test-src-b\"; dest = \"/mnt/b\"; mode = \"rw\"; }
        ];
      };
      uvMountPresent = builtins.any
        (m: m.dest == \"/home/wrix/.cache/uv\")
        extended.profile.mounts;
      hasDest = dest: builtins.any (m: m.dest == dest) extended.profile.mounts;
    in {
      baselineLen = builtins.length baseline.profile.mounts;
      extendedLen = builtins.length extended.profile.mounts;
      uvPresent = uvMountPresent;
      hasA = hasDest \"/mnt/a\";
      hasB = hasDest \"/mnt/b\";
    }
  "); then
    fail "nix eval mounts-append expression failed"
    return 1
  fi

  local baseline_len extended_len uv_present has_a has_b
  baseline_len=$(echo "$result" | jq -r '.baselineLen')
  extended_len=$(echo "$result" | jq -r '.extendedLen')
  uv_present=$(echo   "$result" | jq -r '.uvPresent')
  has_a=$(echo        "$result" | jq -r '.hasA')
  has_b=$(echo        "$result" | jq -r '.hasB')

  if (( baseline_len < 1 )); then
    fail "python profile baseline has no mounts — profile.mounts not propagated"
    return 1
  fi
  if (( extended_len != baseline_len + 2 )); then
    fail "adding 2 mounts should grow profile.mounts by 2 (baseline=$baseline_len, extended=$extended_len)"
    return 1
  fi
  if [[ "$uv_present" != "true" ]]; then
    fail "python profile's UV cache mount missing after extension — merge replaced rather than appended"
    return 1
  fi
  if [[ "$has_a" != "true" || "$has_b" != "true" ]]; then
    fail "consumer mounts missing from profile.mounts (hasA=$has_a hasB=$has_b)"
    return 1
  fi
  pass "Extra mounts are appended to profile.mounts (existing profile mounts preserved)"
}

# ============================================================================
# Mount payload (source, dest, mode) round-trips intact through the merge.
# The launcher reads these fields verbatim via mkMountSpecs; a lossy merge
# would silently drop the mode or rewrite the source path.
# ============================================================================
test_mount_payload_preserved() {
  local result
  if ! result=$(eval_expr_json "
    let
      sb = lib.mkSandbox {
        mounts = [
          { source = \"/srv/wrix/test-payload\"; dest = \"/payload/here\"; mode = \"ro\"; }
        ];
      };
      mountAt = dest: builtins.head (
        builtins.filter (m: m.dest == dest) sb.profile.mounts
      );
      m = mountAt \"/payload/here\";
    in {
      inherit (m) source dest mode;
    }
  "); then
    fail "nix eval mount-payload expression failed"
    return 1
  fi

  local src dest mode
  src=$(echo  "$result" | jq -r '.source')
  dest=$(echo "$result" | jq -r '.dest')
  mode=$(echo "$result" | jq -r '.mode')

  if [[ "$src" != "/srv/wrix/test-payload" ]]; then
    fail "mount.source not preserved: expected '/srv/wrix/test-payload', got '$src'"
    return 1
  fi
  if [[ "$dest" != "/payload/here" ]]; then
    fail "mount.dest not preserved: expected '/payload/here', got '$dest'"
    return 1
  fi
  if [[ "$mode" != "ro" ]]; then
    fail "mount.mode not preserved: expected 'ro', got '$mode'"
    return 1
  fi
  pass "Mount payload (source, dest, mode) round-trips intact through merge"
}

# ============================================================================
# Extra env right-merges into profile.env. The image bakes profile.env into
# the container's Env directive (lib/sandbox/image.nix), so any key present
# on the resolved profile.env reaches the container.
# ============================================================================
test_env_right_merge() {
  local result
  if ! result=$(eval_expr_json "
    let
      added = lib.mkSandbox {
        profile = lib.profiles.base;
        env = { WRIX_TEST_KEY = \"wrix_test_value\"; };
      };
      conflict = lib.mkSandbox {
        profile = lib.profiles.python;
        env = { UV_CACHE_DIR = \"/custom/uv/override\"; };
      };
    in {
      addedValue    = added.profile.env.WRIX_TEST_KEY    or null;
      conflictValue = conflict.profile.env.UV_CACHE_DIR    or null;
    }
  "); then
    fail "nix eval env-merge expression failed"
    return 1
  fi

  local added_value conflict_value
  added_value=$(echo    "$result" | jq -r '.addedValue')
  conflict_value=$(echo "$result" | jq -r '.conflictValue')

  if [[ "$added_value" != "wrix_test_value" ]]; then
    fail "env.WRIX_TEST_KEY expected 'wrix_test_value', got '$added_value'"
    return 1
  fi
  if [[ "$conflict_value" != "/custom/uv/override" ]]; then
    fail "consumer env.UV_CACHE_DIR should override python profile's default, got '$conflict_value'"
    return 1
  fi
  pass "Extra env right-merges into profile.env (consumer wins on key collision)"
}

# ============================================================================
# Existing profile env entries survive a merge that doesn't touch them.
# Adds a fresh key; the python profile's UV_CACHE_DIR must remain intact.
# Guards against // being swapped for an overwriting `=` in the future.
# ============================================================================
test_env_preserves_profile_keys() {
  local result
  if ! result=$(eval_expr_json "
    let
      sb = lib.mkSandbox {
        profile = lib.profiles.python;
        env = { WRIX_EXTRA = \"x\"; };
      };
    in {
      uvValue = sb.profile.env.UV_CACHE_DIR or null;
      extraValue = sb.profile.env.WRIX_EXTRA or null;
    }
  "); then
    fail "nix eval env-preserve expression failed"
    return 1
  fi

  local uv_value extra_value
  uv_value=$(echo    "$result" | jq -r '.uvValue')
  extra_value=$(echo "$result" | jq -r '.extraValue')

  if [[ "$uv_value" != "/home/wrix/.cache/uv" ]]; then
    fail "python profile's UV_CACHE_DIR clobbered by merge: got '$uv_value'"
    return 1
  fi
  if [[ "$extra_value" != "x" ]]; then
    fail "consumer env.WRIX_EXTRA missing from profile.env: got '$extra_value'"
    return 1
  fi
  pass "Profile env keys survive a merge that adds an unrelated key"
}

# ============================================================================
# Default consumer inputs (mounts=[], env={}) leave the profile unchanged.
# Establishes the no-op baseline the other tests compare against.
# ============================================================================
test_default_inputs_preserve_profile() {
  local result
  if ! result=$(eval_expr_json "
    let
      profile = lib.profiles.python;
      sb = lib.mkSandbox { inherit profile; };
    in {
      profileMounts = builtins.length profile.mounts;
      sandboxMounts = builtins.length sb.profile.mounts;
      profileEnvKeys = builtins.length (builtins.attrNames profile.env);
      sandboxEnvKeys = builtins.length (builtins.attrNames sb.profile.env);
    }
  "); then
    fail "nix eval default-input expression failed"
    return 1
  fi

  local pm sm pe se
  pm=$(echo "$result" | jq -r '.profileMounts')
  sm=$(echo "$result" | jq -r '.sandboxMounts')
  pe=$(echo "$result" | jq -r '.profileEnvKeys')
  se=$(echo "$result" | jq -r '.sandboxEnvKeys')

  if (( sm != pm )); then
    fail "mounts=[] should not change mount count (profile=$pm, sandbox=$sm)"
    return 1
  fi
  if (( se != pe )); then
    fail "env={} should not change env key count (profile=$pe, sandbox=$se)"
    return 1
  fi
  pass "Default mounts=[] and env={} leave the profile's mounts/env unchanged"
}

# ============================================================================
# Every sandbox image carries the wrix CLI so session-close commands like
# `wrix beads push` resolve without entering a devShell or running nix.
# ============================================================================
test_custom_mounts_env_reach_live_launcher() {
  "$SCRIPT_DIR/rust-launcher-live.sh" test_linux_custom_mounts_env_reach_live_launcher
}

test_wrix_cli_added_to_sandbox_profile() {
  local result
  if ! result=$(eval_expr_json "
    let
      hasWrix = packages: builtins.any (p: (p.meta.mainProgram or \"\") == \"wrix\") packages;
      profiles = [ lib.profiles.base lib.profiles.rust lib.profiles.python ];
      sandboxFor = profile: lib.mkSandbox { inherit profile; };
      missing = builtins.filter (profile: !(hasWrix (sandboxFor profile).profile.packages)) profiles;
    in {
      missingProfiles = builtins.map (profile: profile.name) missing;
    }
  "); then
    fail "nix eval sandbox-wrix-cli expression failed"
    return 1
  fi

  local missing
  missing=$(echo "$result" | jq -r '.missingProfiles | join(",")')
  if [[ -n "$missing" ]]; then
    fail "mkSandbox did not add wrix CLI to profiles: $missing"
    return 1
  fi

  local source profile_env uname_s
  if ! source=$(nix build --no-link --print-out-paths --no-warn-dirty "$REPO_ROOT#sandbox.image.source"); then
    fail "building the default sandbox image source failed"
    return 1
  fi
  uname_s=$(uname -s)
  if [[ "$uname_s" = "Linux" ]]; then
    profile_env=$(jq -r '.materialized_roots[]' "$source" | while IFS= read -r root; do
      if [[ -x "$root/bin/wrix" ]]; then
        printf '%s\n' "$root"
        break
      fi
    done)
    if [[ -z "$profile_env" ]]; then
      fail "built sandbox image descriptor has no materialized root with bin/wrix"
      return 1
    fi
  elif [[ ! -s "$source" ]]; then
    fail "built sandbox image source is empty: $source"
    return 1
  fi
  pass "mkSandbox adds the wrix CLI to built-in profiles and the built image source"
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_mounts_appended_to_profile
  test_mount_payload_preserved
  test_env_right_merge
  test_env_preserves_profile_keys
  test_default_inputs_preserve_profile
  test_custom_mounts_env_reach_live_launcher
  test_wrix_cli_added_to_sandbox_profile
)

run_all() {
  local fn rc
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    rc=0
    "$fn" || rc=$?
    if [[ "$rc" -ne 0 && "$rc" -ne 77 ]]; then
      fail "$fn returned $rc without calling fail()"
    fi
  done
  echo
  echo "Results: $PASSED passed, $FAILED failed"
  [[ "$FAILED" -eq 0 ]]
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
