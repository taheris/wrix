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

require_commands() {
  command -v nix >/dev/null 2>&1 || skip "nix not on PATH"
  command -v jq >/dev/null 2>&1 || skip "jq not on PATH"
}

nix_eval_json() {
  local expression="$1"
  nix eval --impure --no-warn-dirty --json --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
    in
      $expression
  "
}

assert_json() {
  local result="$1"
  local predicate="$2"
  local message="$3"
  if ! jq -e "$predicate" <<<"$result" >/dev/null; then
    fail "$message: $result"
  fi
}

test_mksandbox_accepts_documented_parameters() {
  local result
  require_commands

  # shellcheck disable=SC2016 # Nix evaluates interpolation in this expression.
  if ! result=$(nix_eval_json '
    let
      extraPkg = builtins.head lib.profiles.base.packages;
      extraMount = {
        source = "~/.cache/wrix-api-contract";
        dest = "/home/wrix/.cache/wrix-api-contract";
        mode = "rw";
      };
      sandbox = lib.mkSandbox {
        profile = lib.profiles.base;
        cpus = 2;
        memoryMb = 2048;
        deployKey = "api-contract";
        packages = [ extraPkg ];
        mounts = [ extraMount ];
        env = { WRIX_API_CONTRACT = "1"; };
        runtimeSecrets = { WRIX_API_SECRET = "required"; };
        mcp = { };
        mcpRuntime = false;
        agent = "direct";
        agentPkg = flake.inputs.nixpkgs.legacyPackages.${system}.hello;
        agentSettings = { };
      };
      required = [ "package" "image" "launcher" "profile" "devShell" ];
    in {
      required_present = builtins.all (name: builtins.hasAttr name sandbox) required;
      launcher_is_raw_wrix = sandbox.launcher == flake.packages.${system}.wrix;
      package_main_program = sandbox.package.meta.mainProgram or "";
      profile_name = sandbox.profile.name;
      env_value = sandbox.profile.env.WRIX_API_CONTRACT or "";
      runtime_secret_policy = sandbox.profile.runtimeSecrets.WRIX_API_SECRET or "";
      mount_present = builtins.any (
        mount:
          mount.source == extraMount.source
          && mount.dest == extraMount.dest
          && (mount.mode or "ro") == "rw"
      ) sandbox.profile.mounts;
      package_added = builtins.length sandbox.profile.packages > builtins.length lib.profiles.base.packages;
    }
  '); then
    fail "nix eval mkSandbox public API failed"
    return 1
  fi

  assert_json "$result" '
    .required_present == true and
    .launcher_is_raw_wrix == true and
    .package_main_program == "wrix-run" and
    .profile_name == "base" and
    .env_value == "1" and
    .runtime_secret_policy == "required" and
    .mount_present == true and
    .package_added == true
  ' "mkSandbox did not expose its documented public API"
}

test_sandbox_devshell_rejects_binding_overrides() {
  local result
  require_commands

  # shellcheck disable=SC2016 # Nix evaluates interpolation in this expression.
  result=$(nix_eval_json '
    let
      sandbox = lib.mkSandbox { };
      profileOverride = builtins.tryEval ((sandbox.devShell { profile = lib.profiles.base; }).shellHook);
      sandboxOverride = builtins.tryEval ((sandbox.devShell { sandbox = sandbox; }).shellHook);
    in {
      profile_accepted = profileOverride.success;
      sandbox_accepted = sandboxOverride.success;
    }
  ')

  assert_json "$result" '
    .profile_accepted == false and
    .sandbox_accepted == false
  ' "sandbox.devShell accepted a profile or sandbox rebinding"
}

test_static_environment_rejects_credentials() {
  local result
  require_commands

  # shellcheck disable=SC2016 # Nix evaluates interpolation in this expression.
  result=$(nix_eval_json '
    let
      profileEnv = builtins.tryEval (
        (lib.mkSandbox {
          profile = lib.deriveProfile lib.profiles.base {
            env = { OPENAI_API_KEY = "must-not-enter-nix"; };
          };
        }).profile.name
      );
      hostEnv = builtins.tryEval (
        (lib.mkSandbox {
          profile = lib.deriveProfile lib.profiles.base {
            hostEnv = { OPENAI_API_KEY = "must-not-enter-nix"; };
          };
        }).profile.name
      );
      sandboxEnv = builtins.tryEval (
        (lib.mkSandbox { env = { ANTHROPIC_API_KEY = "must-not-enter-nix"; }; }).profile.name
      );
      claudeEnv = builtins.tryEval (
        builtins.deepSeq
          (lib.mkSandbox {
            agent = "claude";
            agentSettings.env = { CLAUDE_CODE_OAUTH_TOKEN = "must-not-enter-nix"; };
          }).image.source
          true
      );
      piEnv = builtins.tryEval (
        builtins.deepSeq
          (lib.mkSandbox {
            agent = "pi";
            agentSettings.env = { OPENAI_API_KEY = "must-not-enter-nix"; };
          }).image.source
          true
      );
    in {
      accepted = builtins.any (result: result.success) [ profileEnv hostEnv sandboxEnv claudeEnv piEnv ];
    }
  ')

  assert_json "$result" '.accepted == false' "a static environment surface accepted credential material"
}

test_environment_schema_rejects_invalid_names_and_policies() {
  local result
  require_commands

  # shellcheck disable=SC2016 # Nix evaluates interpolation in this expression.
  result=$(nix_eval_json '
    let
      invalidSecretName = builtins.tryEval (
        (lib.mkSandbox { runtimeSecrets = { "NOT-AN-ENV-NAME" = "optional"; }; }).profile.name
      );
      invalidSecretPolicy = builtins.tryEval (
        (lib.mkSandbox { runtimeSecrets = { WRIX_API_SECRET = "sometimes"; }; }).profile.name
      );
      invalidStaticName = builtins.tryEval (
        (lib.mkSandbox { env = { "OPENAI_API_KEY=shadow" = "canary"; }; }).profile.name
      );
    in {
      accepted = builtins.any (result: result.success) [
        invalidSecretName
        invalidSecretPolicy
        invalidStaticName
      ];
    }
  ')

  assert_json "$result" '.accepted == false' "an invalid environment name or runtime-secret policy was accepted"
}

test_bootstrap_sensitive_environment_is_rejected() {
  local result
  require_commands

  # shellcheck disable=SC2016 # Nix evaluates interpolation in this expression.
  result=$(nix_eval_json '
    let
      names = [
        "BASH_ENV"
        "LD_PRELOAD"
        "PATH"
        "WRIX_NETWORK_LOCAL_ENDPOINTS"
        "WRIX_NETWORK_DNS_SERVERS"
        "BEADS_DOLT_SERVER_HOST"
        "WRIX_PROJECT_CACHE_PORT"
      ];
      staticResults = builtins.map (
        name:
          builtins.tryEval (
            (lib.mkSandbox {
              env = builtins.listToAttrs [ { inherit name; value = "must-not-reach-bootstrap"; } ];
            }).profile.name
          )
      ) names;
      runtimeSecret = builtins.tryEval (
        (lib.mkSandbox { runtimeSecrets = { BASH_ENV = "optional"; }; }).profile.name
      );
      agentSetting = builtins.tryEval (
        builtins.deepSeq
          (lib.mkSandbox {
            agent = "claude";
            agentSettings.env = { LD_PRELOAD = "/workspace/agent.so"; };
          }).image.source
          true
      );
    in {
      accepted = builtins.any (result: result.success) (staticResults ++ [ runtimeSecret agentSetting ]);
    }
  ')

  assert_json "$result" '.accepted == false' "a bootstrap-sensitive environment channel was accepted"
}

ALL_TESTS=(
  test_mksandbox_accepts_documented_parameters
  test_sandbox_devshell_rejects_binding_overrides
  test_static_environment_rejects_credentials
  test_environment_schema_rejects_invalid_names_and_policies
  test_bootstrap_sensitive_environment_is_rejected
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
