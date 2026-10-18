#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP=$(mktemp -d -t wrix-profile-config.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

CONFIG_LINK="$TEST_TMP/profile-config"
WRIX_LINK="$TEST_TMP/host-wrix"
WORKSPACE="$TEST_TMP/workspace"
OPTIONAL_MOUNT_SOURCE="$TEST_TMP/missing-optional-mount"
REQUIRED_MOUNT_SOURCE="$TEST_TMP/present-required-mount"
mkdir -p "$WORKSPACE" "$REQUIRED_MOUNT_SOURCE"

case "$(uname -s)" in
  Darwin) EXPECTED_SOURCE_KIND=docker-archive; ALTERNATE_SOURCE_KIND=nix-descriptor ;;
  *) EXPECTED_SOURCE_KIND=nix-descriptor; ALTERNATE_SOURCE_KIND=docker-archive ;;
esac

prepare() {
  if [[ -f "$CONFIG_LINK" && -x "$WRIX_LINK/bin/wrix" ]]; then
    return
  fi
  nix build --impure --no-warn-dirty --out-link "$CONFIG_LINK" --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      lib = flake.legacyPackages.\${builtins.currentSystem}.lib;
      profile = lib.deriveProfile lib.profiles.base {
        runtimeSecrets = { WRIX_RUNTIME_CONTRACT_TOKEN = \"optional\"; };
        mounts = [
          { source = \"$OPTIONAL_MOUNT_SOURCE\"; dest = \"/mnt/optional-cache\"; mode = \"rw\"; optional = true; }
          { source = \"$REQUIRED_MOUNT_SOURCE\"; dest = \"/mnt/required-cache\"; mode = \"rw\"; optional = false; }
        ];
      };
    in (lib.mkSandbox { inherit profile; }).image.profileConfig
  "
  nix build --no-warn-dirty --out-link "$WRIX_LINK" "$REPO_ROOT#wrix"
  printf '{}\n' >"$TEST_TMP/auth.json"
  export WRIX_PI_AUTH_FILE="$TEST_TMP/auth.json"
}

test_profile_config_contains_launcher_contract_fields() {
  prepare
  jq -e --arg kind "$EXPECTED_SOURCE_KIND" \
    --arg optional "$OPTIONAL_MOUNT_SOURCE" --arg required "$REQUIRED_MOUNT_SOURCE" '
    .schema == 1 and
    (.system | type == "string") and
    (.profile.name | type == "string") and
    (.profile.env | type == "object") and
    (.profile.mounts == [
      {source: $optional, dest: "/mnt/optional-cache", mode: "rw", optional: true},
      {source: $required, dest: "/mnt/required-cache", mode: "rw", optional: false}
    ]) and
    (.profile.writable_dirs | type == "array") and
    (.profile.network_allowlist | type == "array") and
    (.image.ref | type == "string" and length > 0) and
    (.image.source | startswith("/nix/store/")) and
    (.image.source_kind == $kind) and
    (.image.digest | startswith("sha256:")) and
    (.agent.kind == "pi") and
    (.resources.memory_mb | type == "number") and
    (.resources.pids_limit | type == "number") and
    (.security.runtime_secrets == {
      ANTHROPIC_API_KEY: "optional", CLAUDE_CODE_OAUTH_TOKEN: "optional",
      OPENAI_API_KEY: "optional", WRIX_RUNTIME_CONTRACT_TOKEN: "optional"
    }) and
    (.network.default_mode == "open") and (.network.ipv6 == "disabled") and
    (.services.beads.enable == "auto") and (.services.nix_cache.enable == true) and
    (.features.mcp_runtime == false)
  ' "$CONFIG_LINK" >/dev/null
}

test_image_source_kind() {
  prepare
  local missing="$TEST_TMP/missing-kind.json" incompatible="$TEST_TMP/incompatible-kind.json"
  local err="$TEST_TMP/kind.err" config
  jq -e --arg kind "$EXPECTED_SOURCE_KIND" '.image.source_kind == $kind' "$CONFIG_LINK" >/dev/null
  jq 'del(.image.source_kind)' "$CONFIG_LINK" >"$missing"
  jq --arg kind "$ALTERNATE_SOURCE_KIND" '.image.source_kind = $kind' "$CONFIG_LINK" >"$incompatible"
  for config in "$missing" "$incompatible"; do
    if "$WRIX_LINK/bin/wrix" --profile-config "$config" run --dry-run --json "$WORKSPACE" >"$TEST_TMP/kind.out" 2>"$err"; then
      printf 'unsupported image source kind was accepted\n' >&2
      return 1
    fi
    jq -e '.plan == null and .findings[0].code == "invalid_input"' "$TEST_TMP/kind.out" >/dev/null
  done
}

test_host_cli_uses_generated_launch_data() {
  prepare
  local config spawn="$TEST_TMP/spawn.json" out="$TEST_TMP/plan.out" mode
  config=$(readlink -f "$CONFIG_LINK")
  [[ "$config" == /nix/store/* && -f "$config" ]]
  jq -n --arg workspace "$WORKSPACE" '{workspace: $workspace, env: [], agent_args: [], mounts: []}' >"$spawn"
  for mode in run spawn; do
    if [[ "$mode" == run ]]; then
      "$WRIX_LINK/bin/wrix" --profile-config "$config" run --dry-run --json "$WORKSPACE" >"$out"
    else
      "$WRIX_LINK/bin/wrix" --profile-config "$config" spawn --dry-run --json --spawn-config "$spawn" --stdio >"$out"
    fi
    jq -e --arg config "$config" --arg workspace "$WORKSPACE" '.schema == 1 and .plan.profile_config == $config and .plan.workspace == $workspace and .plan.agent == "pi"' "$out" >/dev/null
  done
  WRIX_PROJECT_PROFILE_CONFIG="$config" "$WRIX_LINK/bin/wrix" service --help >"$TEST_TMP/service.out"
  grep -qF 'Usage: wrix service <command>' "$TEST_TMP/service.out"
  "$WRIX_LINK/bin/wrix" >"$TEST_TMP/help.out"
  grep -qF 'Usage: wrix <command>' "$TEST_TMP/help.out"
}

test_profile_config_wrapper_contract() {
  test_profile_config_contains_launcher_contract_fields
  test_host_cli_uses_generated_launch_data
}

if [[ $# -eq 0 ]]; then
  test_profile_config_wrapper_contract
  test_image_source_kind
else
  "$1"
fi
