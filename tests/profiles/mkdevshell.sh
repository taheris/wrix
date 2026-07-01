#!/usr/bin/env bash
# Verify wrix.mkDevShell composition rules (specs/profiles.md § mkDevShell).
#
#   test_profile_required
#     mkDevShell {} (no profile) errors at evaluation — no two-arg fallback.
#
#   test_profile_shellhook_spliced
#     mkDevShell { profile = profiles.rust; } exports the rust profile's
#     shellHook values when the generated hook is sourced.
#
#   test_packages_merge
#     mkDevShell { profile; packages = [extra]; } makes both profile tools
#     and extra tools resolvable on PATH.
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
TMP_BASE="$(mktemp -d)"
trap 'rm -rf "$TMP_BASE"' EXIT

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

write_fake_devshell_tools() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"
  cat > "$bin_dir/wrix" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "service" && "${2:-}" == "start" ]]; then
  exit 0
fi
if [[ "${1:-}" == "service" && "${2:-}" == "endpoints" ]]; then
  printf '%s\n' '{"cache_http":null}'
  exit 0
fi
echo "fake wrix: unexpected args: $*" >&2
exit 2
SCRIPT
  cat > "$bin_dir/nix" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "config" && "${2:-}" == "show" ]]; then
  printf '%s\n' "${NIX_CONFIG:-}"
  exit 0
fi
if [[ "${1:-}" == "show-config" ]]; then
  printf '%s\n' "${NIX_CONFIG:-}"
  exit 0
fi
echo "fake nix: unexpected args: $*" >&2
exit 2
SCRIPT
  cat > "$bin_dir/nix-store" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
exit 0
SCRIPT
  cat > "$bin_dir/host-nix-config.sh" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' 'extra-substituters = file:///tmp/wrix-test-cache'
SCRIPT
  chmod +x "$bin_dir/wrix" "$bin_dir/nix" "$bin_dir/nix-store" "$bin_dir/host-nix-config.sh"
}

source_hook_env() {
  local hook="$1"
  local tmp hook_file bin_dir
  tmp=$(mktemp -d -p "$TMP_BASE")
  hook_file="$tmp/shell-hook.sh"
  bin_dir="$tmp/bin"
  write_fake_devshell_tools "$bin_dir"
  sed -E "s|/nix/store/[[:alnum:]]+-wrix-host-nix-config\.sh|$bin_dir/host-nix-config.sh|g" <<<"$hook" > "$hook_file"
  mkdir -p "$tmp/home" "$tmp/workspace"
  (
    cd "$tmp/workspace"
    export HOME="$tmp/home"
    export PATH="$bin_dir:$PATH"
    export WRIX_BIN="$bin_dir/wrix"
    export SCCACHE_DIR="/home/wrix/.cache/sccache"
    unset CARGO_BUILD_RUSTC_WRAPPER CARGO_INCREMENTAL NIX_CONFIG RUSTC_WRAPPER SCCACHE_CACHE_SIZE
    # shellcheck source=/dev/null
    source "$hook_file" >/dev/null
    env
  )
}

env_value() {
  local env_output="$1"
  local key="$2"
  awk -v key="$key" 'BEGIN { FS = "=" } $1 == key { sub("^[^=]*=", ""); print; exit }' <<<"$env_output"
}

# ============================================================================
# mkDevShell requires `profile` — no default, no two-arg fallback.
# ============================================================================
test_profile_required() {
  if eval_expr_raw "(lib.mkDevShell {}).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: lib.mkDevShell {} should error at evaluation (missing profile)" >&2
    return 1
  fi
  if eval_expr_raw "let sandbox = lib.mkSandbox { profile = lib.profiles.base; }; in (lib.mkDevShell { inherit sandbox; profile = lib.profiles.base; }).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: lib.mkDevShell should reject simultaneous sandbox and profile" >&2
    return 1
  fi
}

# ============================================================================
# rust profile shellHook exports the expected values when sourced.
# ============================================================================
test_profile_shellhook_spliced() {
  local hook env_output rustc_wrapper cargo_wrapper sccache_dir sccache_size cargo_incremental
  if ! hook=$(eval_expr_raw "(lib.mkDevShell { profile = lib.profiles.rust; }).shellHook"); then
    echo "FAIL: mkDevShell { profile = profiles.rust; } evaluation failed" >&2
    return 1
  fi
  if ! env_output=$(source_hook_env "$hook"); then
    echo "FAIL: generated shellHook failed when sourced" >&2
    return 1
  fi

  rustc_wrapper=$(env_value "$env_output" RUSTC_WRAPPER)
  cargo_wrapper=$(env_value "$env_output" CARGO_BUILD_RUSTC_WRAPPER)
  sccache_dir=$(env_value "$env_output" SCCACHE_DIR)
  sccache_size=$(env_value "$env_output" SCCACHE_CACHE_SIZE)
  cargo_incremental=$(env_value "$env_output" CARGO_INCREMENTAL)

  [[ "$rustc_wrapper" == */bin/sccache ]] || {
    echo "FAIL: RUSTC_WRAPPER should export a sccache binary, got '$rustc_wrapper'" >&2
    return 1
  }
  [[ "$cargo_wrapper" == "$rustc_wrapper" ]] || {
    echo "FAIL: CARGO_BUILD_RUSTC_WRAPPER should match RUSTC_WRAPPER, got '$cargo_wrapper'" >&2
    return 1
  }
  [[ "$sccache_dir" == "$TMP_BASE"/*/home/.cache/sccache ]] || {
    echo "FAIL: SCCACHE_DIR should default under HOME/.cache/sccache, got '$sccache_dir'" >&2
    return 1
  }
  [[ "$sccache_size" == "50G" ]] || {
    echo "FAIL: SCCACHE_CACHE_SIZE should export 50G, got '$sccache_size'" >&2
    return 1
  }
  [[ "$cargo_incremental" == "0" ]] || {
    echo "FAIL: CARGO_INCREMENTAL should export 0, got '$cargo_incremental'" >&2
    return 1
  }
}

# ============================================================================
# packages = profile.packages ++ packages, visible through the dev env PATH
# ============================================================================
test_packages_merge() {
  local env_file result profile_cmd extra_cmd profile_output extra_output
  env_file="$TMP_BASE/mkdevshell-packages-env.sh"

  if ! nix print-dev-env --impure --no-warn-dirty --expr "
    let
      system = builtins.currentSystem;
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      pkgs = import flake.inputs.nixpkgs { inherit system; };
      stub = pkgs.writeShellScriptBin \"wrix-mkdevshell-stub\" ''
        set -euo pipefail
        exit 0
      '';
      profileTool = pkgs.writeShellScriptBin \"wrix-mkdevshell-profile-tool\" ''
        set -euo pipefail
        printf '%s\\n' profile-tool
      '';
      extraTool = pkgs.writeShellScriptBin \"wrix-mkdevshell-extra-tool\" ''
        set -euo pipefail
        printf '%s\\n' extra-tool
      '';
      devshell = import $REPO_ROOT/lib/devshell/default.nix {
        inherit pkgs;
        rustCli = {
          wrix = stub;
          cacheHook = stub;
          cachePublish = stub;
        };
        beads = {
          shellHook = \"\";
          waitAndExport = \"\";
        };
      };
      profile = {
        name = \"mkdevshell-test\";
        packages = [ profileTool ];
        hostPackages = [ profileTool ];
        env = { };
        shellHook = \"\";
        mounts = [ ];
        networkAllowlist = [ ];
        enabledPlugins = { };
        writableDirs = [ ];
      };
    in devshell.mkDevShell {
      inherit profile;
      packages = [ extraTool ];
      nixCache = false;
      prekHooks = false;
    }
  " >"$env_file"; then
    echo "FAIL: nix print-dev-env mkDevShell packages-merge expression failed" >&2
    return 1
  fi

  if ! result=$(
    set +u
    # shellcheck source=/dev/null
    source "$env_file" >/dev/null
    set -u
    profile_cmd=$(command -v wrix-mkdevshell-profile-tool)
    extra_cmd=$(command -v wrix-mkdevshell-extra-tool)
    profile_output=$(wrix-mkdevshell-profile-tool)
    extra_output=$(wrix-mkdevshell-extra-tool)
    printf 'profile_cmd=%s\n' "$profile_cmd"
    printf 'extra_cmd=%s\n' "$extra_cmd"
    printf 'profile_output=%s\n' "$profile_output"
    printf 'extra_output=%s\n' "$extra_output"
  ); then
    echo "FAIL: generated mkDevShell environment did not expose both tools on PATH" >&2
    return 1
  fi

  profile_cmd=$(env_value "$result" profile_cmd)
  extra_cmd=$(env_value "$result" extra_cmd)
  profile_output=$(env_value "$result" profile_output)
  extra_output=$(env_value "$result" extra_output)

  [[ "$profile_cmd" == /nix/store/*/bin/wrix-mkdevshell-profile-tool ]] || {
    echo "FAIL: profile tool did not resolve from the Nix store PATH, got '$profile_cmd'" >&2
    return 1
  }
  [[ "$extra_cmd" == /nix/store/*/bin/wrix-mkdevshell-extra-tool ]] || {
    echo "FAIL: extra tool did not resolve from the Nix store PATH, got '$extra_cmd'" >&2
    return 1
  }
  [[ "$profile_output" == "profile-tool" ]] || {
    echo "FAIL: profile tool output mismatch: '$profile_output'" >&2
    return 1
  }
  [[ "$extra_output" == "extra-tool" ]] || {
    echo "FAIL: extra tool output mismatch: '$extra_output'" >&2
    return 1
  }
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
