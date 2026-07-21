#!/usr/bin/env bash
# Verifier for tmux-mcp's mkSandbox composition (specs/tmux-mcp.md).
#
# Builds explicit `mcp.tmux` composition, verifies both entrypoints' runtime
# registration path through the selected Claude CLI, and exercises the built
# image in rootless podman when a host container runtime is available.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"
# shellcheck source=tests/lib/podman-image.sh
source "$REPO_ROOT/tests/lib/podman-image.sh"

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    fail "$command_name not on PATH"
  fi
}

uname_s="$(uname -s)"
[[ "$uname_s" == "Linux" ]] || fail "Linux-only verifier (uname=$uname_s)"
require_command nix
require_command podman
require_command skopeo

env REPO_ROOT="$REPO_ROOT" bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" \
  test_runtime_mcp_registration_uses_claude_user_config_both_entrypoints

PODMAN_BIN="$(command -v podman)"
SKOPEO_BIN="$(command -v skopeo)"
NESTED_CONTAINER=0
PODMAN_COMMAND=("$PODMAN_BIN")
PODMAN_RUN_OPTIONS=(--network=none --cap-add=NET_ADMIN)
MCP_HEALTH_TIMEOUT=10
build_log="$(mktemp -t wrix-e2e-sandbox-build.XXXXXX)"
CONTAINER_NAME="wrix-test-tmux-e2e-sandbox-$$"
IMAGE_REF=""

podman() {
  "${PODMAN_COMMAND[@]}" "$@"
}

skopeo() {
  "$SKOPEO_BIN" "$@"
}

cleanup() {
  local status="$?"
  trap - EXIT
  rm -f "$build_log"
  if [[ -n "$IMAGE_REF" ]] && podman container exists "$CONTAINER_NAME"; then
    if ! podman rm --force "$CONTAINER_NAME" >/dev/null; then
      printf 'WARN: could not remove test container %s\n' "$CONTAINER_NAME" >&2
    fi
  fi
  if [[ -n "$IMAGE_REF" ]] && podman image exists "$IMAGE_REF"; then
    if ! podman rmi "$IMAGE_REF" >/dev/null; then
      printf 'WARN: could not remove test image %s\n' "$IMAGE_REF" >&2
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

verify_claude_mcp_health() {
  local label="$1"
  local selected_agent="$2"
  shift 2
  local result

  if ! result=$(timeout "$MCP_HEALTH_TIMEOUT" \
    "${PODMAN_COMMAND[@]}" run "${PODMAN_RUN_OPTIONS[@]}" --rm \
    --name "$CONTAINER_NAME" -e "WRIX_AGENT=$selected_agent" "$@" "$IMAGE_REF" \
    bash -c 'command -v tmux >/dev/null && command -v tmux-mcp >/dev/null && exec claude mcp get tmux' 2>&1); then
    fail "$label Claude MCP health check failed: $result"
  fi

  if [[ "$result" != *"tmux:"* || "$result" != *"Connected"* ]]; then
    fail "$label Claude did not connect to the registered tmux MCP server: $result"
  fi
}

if [[ -e /run/.containerenv ]]; then
  NESTED_CONTAINER=1
fi

cd "$REPO_ROOT"

if ! PACKAGE_PATH=$(nix build --no-link --print-out-paths --no-warn-dirty --impure --expr "
  let
    flake = builtins.getFlake \"git+file://$REPO_ROOT\";
    system = builtins.currentSystem;
    lib = flake.legacyPackages.\${system}.lib;
  in
    (lib.mkSandbox {
      profile = lib.profiles.rust;
      agent = \"claude\";
      mcp.tmux = { };
    }).package
" 2>"$build_log"); then
  cat "$build_log" >&2
  echo "FAIL: nix build explicit mkSandbox mcp.tmux sandbox" >&2
  exit 1
fi
PROFILE_CONFIG=$(grep -oE -- '--profile-config[[:space:]]+[^[:space:]]+' "$PACKAGE_PATH/bin/wrix" | awk '{print $2}' | head -1)
IMAGE_STREAM=$(jq -r '.image.source' "$PROFILE_CONFIG")
WRAPPER_IMAGE_REF=$(jq -r '.image.ref' "$PROFILE_CONFIG")
SELECTED_AGENT=$(jq -r '.agent.kind' "$PROFILE_CONFIG")
SELECTED_SYSTEM=$(jq -r '.system' "$PROFILE_CONFIG")

[[ "$SELECTED_AGENT" == "claude" ]] || {
  echo "FAIL: explicit mcp.tmux sandbox did not select the Claude agent" >&2
  exit 1
}
[[ -n "$SELECTED_SYSTEM" && "$SELECTED_SYSTEM" != "null" ]] || {
  echo "FAIL: explicit mcp.tmux sandbox did not declare its system" >&2
  exit 1
}
[[ -n "$IMAGE_STREAM" && -e "$IMAGE_STREAM" ]] || {
  echo "FAIL: could not extract image.source from $PROFILE_CONFIG" >&2
  exit 1
}
[[ -n "$WRAPPER_IMAGE_REF" ]] || {
  echo "FAIL: could not extract image.ref from $PROFILE_CONFIG" >&2
  exit 1
}

if ! AUDIT_CONFIG=$(nix build --no-link --print-out-paths --no-warn-dirty --impure --expr "
  let
    flake = builtins.getFlake \"git+file://$REPO_ROOT\";
    system = builtins.currentSystem;
    lib = flake.legacyPackages.\${system}.lib;
  in
    (lib.mkSandbox {
      profile = lib.profiles.rust;
      agent = \"claude\";
      mcp.tmux = {
        audit = \"/workspace/.debug-audit.log\";
        auditFull = \"/workspace/.debug-audit\";
      };
    }).image.claudeConfigJson
" 2>>"$build_log"); then
  cat "$build_log" >&2
  echo "FAIL: nix build explicit mkSandbox mcp.tmux audit settings" >&2
  exit 1
fi

if ! jq -e '
  .mcpServers.tmux.command == "tmux-mcp"
  and .mcpServers.tmux.env.TMUX_DEBUG_AUDIT == "/workspace/.debug-audit.log"
  and .mcpServers.tmux.env.TMUX_DEBUG_AUDIT_FULL == "/workspace/.debug-audit"
' "$AUDIT_CONFIG" >/dev/null; then
  echo "FAIL: mcp.tmux audit/auditFull settings not present in Claude user config" >&2
  cat "$AUDIT_CONFIG" >&2
  exit 1
fi

if ! LIVE_AGENT_ENV=$(nix build --no-link --print-out-paths --no-warn-dirty --impure --expr "
  let
    flake = builtins.getFlake \"git+file://$REPO_ROOT\";
    system = \"$SELECTED_SYSTEM\";
    pkgs = import flake.inputs.nixpkgs {
      inherit system;
      config.allowUnfreePredicate = pkg: pkgs.lib.getName pkg == \"claude-code\";
    };
  in
    pkgs.buildEnv {
      name = \"wrix-tmux-mcp-live-agent\";
      paths = [ pkgs.claude-code pkgs.tmux flake.packages.\${system}.tmux-mcp ];
      pathsToLink = [ \"/bin\" ];
    }
" 2>>"$build_log"); then
  cat "$build_log" >&2
  fail "nix build selected Claude and tmux-mcp binaries"
fi
if ! env \
  PATH="$LIVE_AGENT_ENV/bin:$PATH" \
  REPO_ROOT="$REPO_ROOT" \
  WRIX_TEST_MCP_CONFIG="$AUDIT_CONFIG" \
  bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" \
  test_runtime_mcp_registration_is_discovered_by_selected_claude; then
  fail "selected Claude runtime MCP verifier"
fi

if [[ "$NESTED_CONTAINER" -eq 1 ]]; then
  echo "PASS: built explicit tmux-mcp sandbox and verified runtime registration through selected Claude" >&2
  exit 0
fi

IMAGE_REF=$(wrix_unique_image_ref "wrix-test-tmux-e2e-sandbox")
wrix_load_test_image "$IMAGE_STREAM" "$(wrix_image_short_name "$WRAPPER_IMAGE_REF")" "$IMAGE_REF"
verify_claude_mcp_health "explicit mcp.tmux" "$SELECTED_AGENT"

echo "PASS: explicit tmux-mcp sandbox and runtime-selected Claude registration" >&2
