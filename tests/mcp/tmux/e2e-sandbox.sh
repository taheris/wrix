#!/usr/bin/env bash
# Verifier for tmux-mcp's mkSandbox composition (specs/tmux-mcp.md).
#
# Instantiates `mkSandbox { profile = profiles.rust; agent = "claude";
# mcp.tmux = {}; }` directly, loads the resulting image into the host's
# rootless podman, and asserts that Claude discovers the registered server and
# completes its MCP health check against tmux-mcp.

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

PODMAN_BIN="$(command -v podman)"
SKOPEO_BIN="$(command -v skopeo)"
NESTED_RUNTIME_DIR=""
NESTED_UID=""
NESTED_GID=""
NESTED_PODMAN_READY=0
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
  if [[ -z "$NESTED_RUNTIME_DIR" ]]; then
    "$SKOPEO_BIN" "$@"
    return
  fi

  setpriv --reuid="$NESTED_UID" --regid="$NESTED_GID" --clear-groups \
    env CONTAINERS_STORAGE_CONF="$NESTED_RUNTIME_DIR/storage.conf" \
    HOME="$NESTED_RUNTIME_DIR/home" TMPDIR="$NESTED_RUNTIME_DIR/tmp" \
    "$SKOPEO_BIN" "$@"
}

cleanup() {
  local status="$?"
  trap - EXIT
  rm -f "$build_log"
  if podman container exists "$CONTAINER_NAME"; then
    if ! podman rm --force "$CONTAINER_NAME" >/dev/null; then
      printf 'WARN: could not remove test container %s\n' "$CONTAINER_NAME" >&2
    fi
  fi
  if [[ -n "$IMAGE_REF" ]] && podman image exists "$IMAGE_REF"; then
    if ! podman rmi "$IMAGE_REF" >/dev/null; then
      printf 'WARN: could not remove test image %s\n' "$IMAGE_REF" >&2
    fi
  fi
  if [[ "$NESTED_PODMAN_READY" -eq 1 ]]; then
    if ! podman system migrate; then
      printf 'WARN: could not stop nested Podman user namespace\n' >&2
    fi
  fi
  if [[ -n "$NESTED_RUNTIME_DIR" ]]; then
    rm -rf "$NESTED_RUNTIME_DIR"
  fi
  exit "$status"
}
trap cleanup EXIT

if [[ -e /run/.containerenv ]]; then
  require_command setpriv
  if ! NESTED_UID="$(id -u wrix)"; then
    fail "nested Podman requires the wrix runtime user"
  fi
  if ! NESTED_GID="$(id -g wrix)"; then
    fail "nested Podman requires the wrix runtime group"
  fi
  NESTED_RUNTIME_DIR="$(mktemp -d -t wrix-e2e-podman.XXXXXX)"
  mkdir -p "$NESTED_RUNTIME_DIR/home/.config/containers" "$NESTED_RUNTIME_DIR/tmp"
  cat >"$NESTED_RUNTIME_DIR/storage.conf" <<EOF
[storage]
driver = "vfs"
runroot = "$NESTED_RUNTIME_DIR/runroot"
graphroot = "$NESTED_RUNTIME_DIR/graphroot"
EOF
  printf '%s\n' '{"default":[{"type":"insecureAcceptAnything"}]}' \
    >"$NESTED_RUNTIME_DIR/home/.config/containers/policy.json"
  chown -R "$NESTED_UID:$NESTED_GID" "$NESTED_RUNTIME_DIR"
  PODMAN_COMMAND=(
    setpriv --reuid="$NESTED_UID" --regid="$NESTED_GID" --clear-groups
    env CONTAINERS_STORAGE_CONF="$NESTED_RUNTIME_DIR/storage.conf"
    HOME="$NESTED_RUNTIME_DIR/home" TMPDIR="$NESTED_RUNTIME_DIR/tmp"
    "$PODMAN_BIN"
  )
  PODMAN_RUN_OPTIONS+=(--pid=host --ipc=host --uts=host)
  MCP_HEALTH_TIMEOUT=120
  NESTED_PODMAN_READY=1
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

[[ "$SELECTED_AGENT" == "claude" ]] || {
  echo "FAIL: explicit mcp.tmux sandbox did not select the Claude agent" >&2
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

IMAGE_REF=$(wrix_unique_image_ref "wrix-test-tmux-e2e-sandbox")
wrix_load_test_image "$IMAGE_STREAM" "$(wrix_image_short_name "$WRAPPER_IMAGE_REF")" "$IMAGE_REF"

if ! result=$(timeout "$MCP_HEALTH_TIMEOUT" \
  "${PODMAN_COMMAND[@]}" run "${PODMAN_RUN_OPTIONS[@]}" --rm \
  --name "$CONTAINER_NAME" -e "WRIX_AGENT=$SELECTED_AGENT" "$IMAGE_REF" \
  bash -c 'command -v tmux >/dev/null && command -v tmux-mcp >/dev/null && exec claude mcp get tmux' 2>&1); then
  echo "FAIL: Claude MCP health check failed: $result" >&2
  exit 1
fi

[[ "$result" == *"tmux:"* && "$result" == *"Connected"* ]] || {
  echo "FAIL: Claude did not connect to the registered tmux MCP server: $result" >&2
  exit 1
}

echo "PASS: tmux-mcp-e2e-sandbox" >&2
