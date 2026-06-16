#!/usr/bin/env bash
# Verifier for tmux-mcp's mkSandbox composition (specs/tmux-mcp.md).
#
# Builds the sandbox-rust-mcp image (the rust profile with `mcp.tmux = {}`
# threaded through mkSandbox), loads it into the host's rootless podman,
# and asserts:
#   1. tmux resolves on PATH inside the container;
#   2. tmux-mcp resolves on PATH inside the container;
#   3. the MCP server responds to a JSON-RPC `initialize` request.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"
# shellcheck source=tests/lib/podman-image.sh
source "$REPO_ROOT/tests/lib/podman-image.sh"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s)"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[[ -e /run/.containerenv ]] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

# Silence nix-build chatter (streamLayeredImage emits ~hundreds of
# "Creating layer N from paths" lines on stderr) so loom's per-verifier
# output buffer reaches the actual test output before truncating.
build_log=$(mktemp -t wrix-e2e-sandbox-build.XXXXXX)
IMAGE_REF=""
cleanup() {
  rm -f "$build_log"
  if [[ -n "$IMAGE_REF" ]] && podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

if ! PACKAGE_PATH=$(nix build --no-link --print-out-paths --no-warn-dirty .#sandbox-rust-mcp 2>"$build_log"); then
  cat "$build_log" >&2
  echo "FAIL: nix build .#sandbox-rust-mcp" >&2
  exit 1
fi
PROFILE_CONFIG=$(grep -oE -- '--profile-config[[:space:]]+[^[:space:]]+' "$PACKAGE_PATH/bin/wrix" | awk '{print $2}' | head -1)
IMAGE_STREAM=$(jq -r '.image.source' "$PROFILE_CONFIG")
WRAPPER_IMAGE_REF=$(jq -r '.image.ref' "$PROFILE_CONFIG")

[[ -n "$IMAGE_STREAM" && -e "$IMAGE_STREAM" ]] || {
  echo "FAIL: could not extract image.source from $PROFILE_CONFIG" >&2
  exit 1
}
[[ -n "$WRAPPER_IMAGE_REF" ]] || {
  echo "FAIL: could not extract image.ref from $PROFILE_CONFIG" >&2
  exit 1
}

IMAGE_REF=$(wrix_unique_image_ref "wrix-test-tmux-e2e-sandbox")
wrix_load_test_image "$IMAGE_STREAM" "$(wrix_image_short_name "$WRAPPER_IMAGE_REF")" "$IMAGE_REF"

for cmd in tmux tmux-mcp; do
  podman run --rm --entrypoint /bin/bash "$IMAGE_REF" \
    -c "command -v $cmd" >/dev/null || {
    echo "FAIL: $cmd not on PATH inside the container" >&2
    exit 1
  }
done

init_req='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-sandbox","version":"1.0"}}}'
result=$(printf '%s\n' "$init_req" | timeout 10 podman run --rm -i \
  --entrypoint /bin/bash "$IMAGE_REF" \
  -c "tmux-mcp 2>/dev/null" || true)

[[ "$result" == *serverInfo* ]] || {
  echo "FAIL: tmux-mcp did not respond to initialize: $result" >&2
  exit 1
}

echo "PASS: tmux-mcp-e2e-sandbox" >&2
