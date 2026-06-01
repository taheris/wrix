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
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[ -e /run/.containerenv ] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

# Silence nix-build chatter (streamLayeredImage emits ~hundreds of
# "Creating layer N from paths" lines on stderr) so loom's per-verifier
# output buffer reaches the actual test output before truncating.
build_log=$(mktemp -t wrapix-e2e-sandbox-build.XXXXXX)
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

short_name="${IMAGE_REF##*/}"
short_name="${short_name%%:*}"
if podman image exists "$IMAGE_REF"; then
  podman rmi "$IMAGE_REF" >/dev/null
fi
# Retag the image podman just loaded, named from `podman load`'s own
# reported ref. A `podman images --filter reference=*name* | head -n1`
# is non-deterministic on a host carrying images from prior runs and can
# retag a stale build (e.g. one predating tmux-mcp), silently exercising
# the wrong image. `podman load` normalizes a streamLayeredImage's
# manifest tag in a version-dependent way, so read the ref back.
# Capture the stream script's stderr (~hundreds of "Creating layer N
# from paths" lines) to a log so it can be surfaced on failure without
# flooding loom's per-verifier output buffer; capture podman load's
# stdout+stderr to read the loaded ref back out.
stream_log=$(mktemp -t wrapix-e2e-sandbox-stream.XXXXXX)
if ! load_out=$("$IMAGE_STREAM" 2>"$stream_log" | podman load 2>&1); then
  cat "$stream_log" >&2
  rm -f "$stream_log"
  echo "FAIL: podman load" >&2
  printf '%s\n' "$load_out" >&2
  exit 1
fi
rm -f "$stream_log"
loaded_ref=$(printf '%s\n' "$load_out" \
  | sed -n 's/^Loaded image(s): //p; s/^Loaded image: //p' \
  | tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' \
  | grep -F "$short_name" | head -n1)
[[ -n "$loaded_ref" ]] || {
  echo "FAIL: could not determine loaded image ref from podman load output" >&2
  printf '%s\n' "$load_out" >&2
  exit 1
}
podman tag "$loaded_ref" "$IMAGE_REF"

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
