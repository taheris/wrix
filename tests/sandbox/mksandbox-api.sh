#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"
  printf 'SKIP: %s\n' "$reason" >&2
  exit 77
}

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_command() {
  local command_name="$1"
  command -v "$command_name" >/dev/null 2>&1 || skip "$command_name not on PATH"
}

test_mksandbox_accepts_documented_parameters() {
  local result
  require_command nix
  require_command jq

  if ! result=$(nix eval --impure --no-warn-dirty --json --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
      extraPkg = builtins.head lib.profiles.base.packages;
      extraMount = {
        source = \"~/.cache/wrix-api-contract\";
        dest = \"/home/wrix/.cache/wrix-api-contract\";
        mode = \"rw\";
      };
      sandbox = lib.mkSandbox {
        profile = lib.profiles.base;
        cpus = 2;
        memoryMb = 2048;
        deployKey = \"api-contract\";
        packages = [ extraPkg ];
        mounts = [ extraMount ];
        env = { WRIX_API_CONTRACT = \"1\"; };
        mcp = { };
        mcpRuntime = false;
        agent = \"direct\";
        agentPkg = extraPkg;
        agentSettings = { };
      };
      required = [ \"package\" \"image\" \"launcher\" \"profile\" \"devShell\" ];
      shell = sandbox.devShell { };
      profileOverride = builtins.tryEval ((sandbox.devShell { profile = flake.legacyPackages.\${system}.lib.profiles.base; }).shellHook);
      sandboxOverride = builtins.tryEval ((sandbox.devShell { sandbox = sandbox; }).shellHook);
    in
    {
      required_present = builtins.all (name: builtins.hasAttr name sandbox) required;
      launcher_is_raw_wrix = sandbox.launcher == flake.packages.\${system}.wrix;
      package_main_program = sandbox.package.meta.mainProgram or \"\";
      devshell_rejects_profile = profileOverride.success;
      devshell_rejects_sandbox = sandboxOverride.success;
      profile_name = sandbox.profile.name;
      shell_name = shell.name or \"\";
      env_value = sandbox.profile.env.WRIX_API_CONTRACT or \"\";
      mount_present = builtins.any (
        mount:
          mount.source == extraMount.source
          && mount.dest == extraMount.dest
          && (mount.mode or \"ro\") == \"rw\"
      ) sandbox.profile.mounts;
      package_count = builtins.length sandbox.profile.packages;
      base_package_count = builtins.length lib.profiles.base.packages;
    }
  "); then
    fail "nix eval mkSandbox contract expression failed"
    return 1
  fi

  if ! jq -e '
    .required_present == true and
    .launcher_is_raw_wrix == true and
    .package_main_program == "wrix-run" and
    .devshell_rejects_profile == false and
    .devshell_rejects_sandbox == false and
    .profile_name == "base" and
    (.shell_name | type == "string" and length > 0) and
    .env_value == "1" and
    .mount_present == true and
    (.package_count > .base_package_count)
  ' <<<"$result" >/dev/null; then
    fail "mkSandbox did not expose the documented API contract: $result"
    return 1
  fi

  printf 'PASS: mkSandbox accepts documented parameters and exposes raw Rust launcher\n' >&2
}

ALL_TESTS=(
  test_mksandbox_accepts_documented_parameters
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  [[ "$failed" -eq 0 ]]
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
