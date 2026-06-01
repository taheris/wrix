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

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s)"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

cd "$REPO_ROOT"

# Silence nix-build chatter (streamLayeredImage emits ~hundreds of
# "Creating layer N from paths" lines on stderr) so loom's per-verifier
# output buffer reaches the actual test output before truncating.
build_log=$(mktemp -t wrapix-e2e-sandbox-build.XXXXXX)
IMAGE_REF=""
cleanup() {
  rm -f "$build_log"
  [[ -n "$IMAGE_REF" ]] && podman rmi -f "$IMAGE_REF" >/dev/null 2>&1 || true
}
trap cleanup EXIT

if ! PACKAGE_PATH=$(nix build --no-link --print-out-paths --no-warn-dirty .#sandbox-rust-mcp 2>"$build_log"); then
  cat "$build_log" >&2
  echo "FAIL: nix build .#sandbox-rust-mcp" >&2
  exit 1
fi
IMAGE_STREAM=$(grep -oP "WRAPIX_DEFAULT_IMAGE_SOURCE=[^']*'\K[^']+" "$PACKAGE_PATH/bin/wrapix" | head -1)
IMAGE_REF=$(grep -oP "WRAPIX_DEFAULT_IMAGE_REF=[^']*'\K[^']+" "$PACKAGE_PATH/bin/wrapix" | head -1)

[[ -n "$IMAGE_STREAM" && -e "$IMAGE_STREAM" ]] || {
  echo "FAIL: could not extract IMAGE_SOURCE from $PACKAGE_PATH/bin/wrapix" >&2
  exit 1
}
[[ -n "$IMAGE_REF" ]] || {
  echo "FAIL: could not extract IMAGE_REF from $PACKAGE_PATH/bin/wrapix" >&2
  exit 1
}

"$IMAGE_STREAM" | podman load >/dev/null

# podman load stores the unqualified manifest tag under a podman-version-
# dependent ref (`wrapix-rust:latest`, `localhost/wrapix-rust:latest`, or
# `docker.io/library/wrapix-rust:latest` depending on registries.conf
# defaults). Re-tag the loaded image to ${IMAGE_REF} via its ID so
# subsequent `podman run` commands address it unambiguously — same
# pattern as lib/util/shell.nix's imageLoadStep.
short_name="${IMAGE_REF##*/}"
short_name="${short_name%%:*}"
loaded_id=$(podman images --quiet --filter "reference=*${short_name}*" | head -n1)
[[ -n "$loaded_id" ]] || {
  echo "FAIL: image not found after podman load (filter: *${short_name}*)" >&2
  podman images >&2
  exit 1
}
podman tag "$loaded_id" "$IMAGE_REF"

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
