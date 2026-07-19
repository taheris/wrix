#!/usr/bin/env bash
# Verify wrix.mkDevShell composition rules (specs/profiles.md § mkDevShell).
#
#   test_profile_required
#     mkDevShell rejects missing/conflicting profile sources, and sandbox.devShell
#     rejects attempts to override its bound sandbox/profile.
#
#   test_profile_shellhook_spliced
#     mkDevShell { profile = profiles.rust; } exports the rust profile's
#     shellHook values when the generated hook is sourced.
#
#   test_host_packages_source
#     mkDevShell { profile; packages = [extra]; } makes profile.hostPackages
#     and extra tools resolvable on PATH while leaving image packages out.
#
#   test_env_right_merge
#     mkDevShell { profile; env = { K = "v"; }; } preserves consumer values
#     after the generated devshell hook runs, including rust-profile conflicts.
#
#   test_shellhook_order
#     Sourcing the generated hook executes the profile hook before the consumer
#     hook, and both hooks' effects are observable in the resulting shell.
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

apply_env_json() {
  local env_json="$1"
  local name value
  while IFS='=' read -r name value; do
    printf -v "$name" '%s' "$value"
    export "${name?}"
  done < <(jq -r 'to_entries[] | select(.value != null) | "\(.key)=\(.value)"' <<<"$env_json")
}

source_hook_env() {
  local hook="$1"
  local devshell_env="$2"
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
    apply_env_json "$devshell_env"
    unset NIX_CONFIG
    # shellcheck source=/dev/null
    source "$hook_file" >/dev/null
    printf 'RUSTC=%s\n' "${RUSTC-}"
    printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-}"
    printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-}"
    printf 'SCCACHE_DIR=%s\n' "${SCCACHE_DIR-}"
    printf 'SCCACHE_CACHE_SIZE=%s\n' "${SCCACHE_CACHE_SIZE-}"
    printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-}"
    printf 'MKDEVSHELL_TEST_KEY=%s\n' "${MKDEVSHELL_TEST_KEY-}"
    printf 'WRIX_TEST_PROFILE_HOOK_FIRED=%s\n' "${WRIX_TEST_PROFILE_HOOK_FIRED-}"
    printf 'WRIX_TEST_HOOK_ORDER=%s\n' "${WRIX_TEST_HOOK_ORDER-}"
  )
}

env_value() {
  local env_output="$1"
  local key="$2"
  local name value
  while IFS='=' read -r name value; do
    if [[ "$name" == "$key" ]]; then
      printf '%s\n' "$value"
      return 0
    fi
  done <<<"$env_output"
}

# ============================================================================
# mkDevShell/sandbox.devShell reject missing or conflicting profile sources.
# ============================================================================
test_profile_required() {
  local sandbox_expr
  sandbox_expr="let sandbox = lib.mkSandbox { profile = lib.profiles.base; }; in"

  if eval_expr_raw "(lib.mkDevShell {}).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: lib.mkDevShell {} should error at evaluation (missing profile)" >&2
    return 1
  fi
  if eval_expr_raw "$sandbox_expr (lib.mkDevShell { inherit sandbox; profile = lib.profiles.base; }).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: lib.mkDevShell should reject simultaneous sandbox and profile" >&2
    return 1
  fi
  if eval_expr_raw "$sandbox_expr (sandbox.devShell { profile = lib.profiles.base; }).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: sandbox.devShell should reject profile override" >&2
    return 1
  fi
  if eval_expr_raw "$sandbox_expr (sandbox.devShell { sandbox = sandbox; }).shellHook" >/dev/null 2>/dev/null; then
    echo "FAIL: sandbox.devShell should reject sandbox override" >&2
    return 1
  fi
}

# ============================================================================
# rust profile env plus shellHook exports the expected values when sourced.
# ============================================================================
test_profile_shellhook_spliced() {
  local devshell_env hook env_output rustc expected_rustc rustc_wrapper cargo_wrapper sccache_dir sccache_size cargo_incremental
  if ! devshell_env=$(eval_expr_json '
    let shell = lib.mkDevShell { profile = lib.profiles.rust; };
    in {
      CARGO_BUILD_RUSTC_WRAPPER = shell.CARGO_BUILD_RUSTC_WRAPPER or null;
      CARGO_INCREMENTAL = shell.CARGO_INCREMENTAL or null;
      RUSTC = shell.RUSTC or null;
      RUSTC_WRAPPER = shell.RUSTC_WRAPPER or null;
      SCCACHE_CACHE_SIZE = shell.SCCACHE_CACHE_SIZE or null;
      SCCACHE_DIR = shell.SCCACHE_DIR or null;
    }
  '); then
    echo "FAIL: mkDevShell { profile = profiles.rust; } env evaluation failed" >&2
    return 1
  fi
  if ! hook=$(eval_expr_raw "(lib.mkDevShell { profile = lib.profiles.rust; }).shellHook"); then
    echo "FAIL: mkDevShell { profile = profiles.rust; } shellHook evaluation failed" >&2
    return 1
  fi
  if ! env_output=$(source_hook_env "$hook" "$devshell_env"); then
    echo "FAIL: generated shellHook failed when sourced" >&2
    return 1
  fi

  rustc=$(env_value "$env_output" RUSTC)
  expected_rustc=$(eval_expr_raw '"${lib.profiles.rust.toolchain}/bin/rustc"')
  rustc_wrapper=$(env_value "$env_output" RUSTC_WRAPPER)
  cargo_wrapper=$(env_value "$env_output" CARGO_BUILD_RUSTC_WRAPPER)
  sccache_dir=$(env_value "$env_output" SCCACHE_DIR)
  sccache_size=$(env_value "$env_output" SCCACHE_CACHE_SIZE)
  cargo_incremental=$(env_value "$env_output" CARGO_INCREMENTAL)

  [[ "$rustc" == "$expected_rustc" ]] || {
    echo "FAIL: RUSTC should select the profile toolchain, got '$rustc'" >&2
    return 1
  }
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
# profile.hostPackages ++ packages is visible through the dev env PATH
# ============================================================================
test_host_packages_source() {
  local env_file result host_cmd extra_cmd image_found host_output extra_output
  env_file="$TMP_BASE/mkdevshell-host-packages-env.sh"

  if ! nix print-dev-env --impure --no-warn-dirty --expr "
    let
      system = builtins.currentSystem;
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      pkgs = import flake.inputs.nixpkgs { inherit system; };
      stub = pkgs.writeShellScriptBin \"wrix-mkdevshell-stub\" ''
        set -euo pipefail
        exit 0
      '';
      imageTool = pkgs.writeShellScriptBin \"wrix-mkdevshell-image-tool\" ''
        set -euo pipefail
        printf '%s\\n' image-tool
      '';
      hostTool = pkgs.writeShellScriptBin \"wrix-mkdevshell-host-tool\" ''
        set -euo pipefail
        printf '%s\\n' host-tool
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
        packages = [ imageTool ];
        hostPackages = [ hostTool ];
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
    echo "FAIL: nix print-dev-env mkDevShell hostPackages expression failed" >&2
    return 1
  fi

  if ! result=$(
    set +u
    # shellcheck source=/dev/null
    source "$env_file" >/dev/null
    set -u
    host_cmd=$(command -v wrix-mkdevshell-host-tool)
    extra_cmd=$(command -v wrix-mkdevshell-extra-tool)
    if command -v wrix-mkdevshell-image-tool >/dev/null; then
      image_found=1
    else
      image_found=0
    fi
    host_output=$(wrix-mkdevshell-host-tool)
    extra_output=$(wrix-mkdevshell-extra-tool)
    printf 'host_cmd=%s\n' "$host_cmd"
    printf 'extra_cmd=%s\n' "$extra_cmd"
    printf 'image_found=%s\n' "$image_found"
    printf 'host_output=%s\n' "$host_output"
    printf 'extra_output=%s\n' "$extra_output"
  ); then
    echo "FAIL: generated mkDevShell environment did not expose expected tools on PATH" >&2
    return 1
  fi

  host_cmd=$(env_value "$result" host_cmd)
  extra_cmd=$(env_value "$result" extra_cmd)
  image_found=$(env_value "$result" image_found)
  host_output=$(env_value "$result" host_output)
  extra_output=$(env_value "$result" extra_output)

  [[ "$host_cmd" == /nix/store/*/bin/wrix-mkdevshell-host-tool ]] || {
    echo "FAIL: host tool did not resolve from the Nix store PATH, got '$host_cmd'" >&2
    return 1
  }
  [[ "$extra_cmd" == /nix/store/*/bin/wrix-mkdevshell-extra-tool ]] || {
    echo "FAIL: extra tool did not resolve from the Nix store PATH, got '$extra_cmd'" >&2
    return 1
  }
  [[ "$image_found" == "0" ]] || {
    echo "FAIL: image-only profile package should not be present on the host PATH" >&2
    return 1
  }
  [[ "$host_output" == "host-tool" ]] || {
    echo "FAIL: host tool output mismatch: '$host_output'" >&2
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
  local devshell_env hook env_output added_value rustc rustc_wrapper cargo_wrapper
  if ! devshell_env=$(eval_expr_json '
    let shell = lib.mkDevShell {
      profile = lib.profiles.rust;
      env = {
        MKDEVSHELL_TEST_KEY = "mkdevshell_test_value";
        RUSTC = "/consumer/rustc";
        RUSTC_WRAPPER = "/consumer/rustc-wrapper";
        CARGO_BUILD_RUSTC_WRAPPER = "/consumer/cargo-rustc-wrapper";
      };
    };
    in {
      CARGO_BUILD_RUSTC_WRAPPER = shell.CARGO_BUILD_RUSTC_WRAPPER or null;
      MKDEVSHELL_TEST_KEY = shell.MKDEVSHELL_TEST_KEY or null;
      RUSTC = shell.RUSTC or null;
      RUSTC_WRAPPER = shell.RUSTC_WRAPPER or null;
    }
  '); then
    echo "FAIL: nix eval mkDevShell env-merge expression failed" >&2
    return 1
  fi
  if ! hook=$(eval_expr_raw '(lib.mkDevShell {
    profile = lib.profiles.rust;
    env = {
      MKDEVSHELL_TEST_KEY = "mkdevshell_test_value";
      RUSTC = "/consumer/rustc";
      RUSTC_WRAPPER = "/consumer/rustc-wrapper";
      CARGO_BUILD_RUSTC_WRAPPER = "/consumer/cargo-rustc-wrapper";
    };
  }).shellHook'); then
    echo "FAIL: mkDevShell env-merge shellHook evaluation failed" >&2
    return 1
  fi
  if ! env_output=$(source_hook_env "$hook" "$devshell_env"); then
    echo "FAIL: generated env-merge shellHook failed when sourced" >&2
    return 1
  fi

  added_value=$(env_value "$env_output" MKDEVSHELL_TEST_KEY)
  rustc=$(env_value "$env_output" RUSTC)
  rustc_wrapper=$(env_value "$env_output" RUSTC_WRAPPER)
  cargo_wrapper=$(env_value "$env_output" CARGO_BUILD_RUSTC_WRAPPER)

  [[ "$added_value" == "mkdevshell_test_value" ]] || {
    echo "FAIL: env.MKDEVSHELL_TEST_KEY expected 'mkdevshell_test_value', got '$added_value'" >&2
    return 1
  }
  [[ "$rustc" == "/consumer/rustc" ]] || {
    echo "FAIL: consumer RUSTC was overwritten after shellHook execution: '$rustc'" >&2
    return 1
  }
  [[ "$rustc_wrapper" == "/consumer/rustc-wrapper" ]] || {
    echo "FAIL: consumer RUSTC_WRAPPER was overwritten after shellHook execution: '$rustc_wrapper'" >&2
    return 1
  }
  [[ "$cargo_wrapper" == "/consumer/cargo-rustc-wrapper" ]] || {
    echo "FAIL: consumer CARGO_BUILD_RUSTC_WRAPPER was overwritten after shellHook execution: '$cargo_wrapper'" >&2
    return 1
  }
}

# ============================================================================
# shellHook order: lifecycle → profile.shellHook → consumer shellHook
# ============================================================================
test_shellhook_order() {
  local hook env_output profile_fired final_order
  if ! hook=$(eval_expr_raw '
    let
      profile = lib.deriveProfile lib.profiles.base {
        shellHook = "export WRIX_TEST_PROFILE_HOOK_FIRED=1; export WRIX_TEST_HOOK_ORDER=profile";
      };
    in (lib.mkDevShell {
      inherit profile;
      shellHook = "export WRIX_TEST_HOOK_ORDER=consumer";
    }).shellHook
  '); then
    echo "FAIL: mkDevShell shellHook-order evaluation failed" >&2
    return 1
  fi
  if ! env_output=$(source_hook_env "$hook" '{}'); then
    echo "FAIL: generated shellHook failed when sourced" >&2
    return 1
  fi

  profile_fired=$(env_value "$env_output" WRIX_TEST_PROFILE_HOOK_FIRED)
  final_order=$(env_value "$env_output" WRIX_TEST_HOOK_ORDER)

  [[ "$profile_fired" == "1" ]] || {
    echo "FAIL: profile shellHook did not execute" >&2
    return 1
  }
  [[ "$final_order" == "consumer" ]] || {
    echo "FAIL: consumer shellHook did not execute last: '$final_order'" >&2
    return 1
  }
}

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_profile_required
  test_profile_shellhook_spliced
  test_host_packages_source
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
