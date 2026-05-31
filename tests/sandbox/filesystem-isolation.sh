#!/usr/bin/env bash
# Verifier for criterion 108 of specs/sandbox.md:
#
#   Host filesystem outside /workspace and declared mounts is not visible
#   inside the container.
#
# Runs directly against the host's rootless podman: builds the test sandbox
# image, loads it, then runs a container that:
#   1. CAN read $WORKSPACE/testfile.txt via the -v bind mount;
#   2. CANNOT read a sentinel file on the host (placed outside the bind);
#   3. CANNOT see host users in /etc/passwd (image's fakeNss owns it).
#
#   Linux + rootless podman + nix  -> exercise the image
#   Darwin                         -> exit 77 (macOS path covered by tests/darwin/*)
#   non-Linux non-Darwin           -> exit 77
#   nix or podman missing          -> exit 77

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); macOS covered by tests/darwin/*"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-fs-isolation.XXXXXX)
HOST_SENTINEL=$(mktemp -t wrapix-fs-isolation-host-secret.XXXXXX)
echo 'host-secret' > "$HOST_SENTINEL"
echo 'workspace-content' > "$WORKSPACE/testfile.txt"

cleanup() {
  rm -rf "$WORKSPACE" "$HOST_SENTINEL"
  podman rmi -f localhost/wrapix-base:latest >/dev/null 2>&1 || true
}
trap cleanup EXIT

"$IMAGE_STREAM" | podman load >/dev/null
IMAGE_REF="localhost/wrapix-base:latest"

# 1. Workspace bind mount is readable.
result=$(podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  -w /workspace \
  "$IMAGE_REF" \
  -c "cat /workspace/testfile.txt")
[[ "$result" == *workspace-content* ]] || {
  echo "FAIL: workspace bind mount not readable: $result" >&2
  exit 1
}

# 2. The host's sentinel path is NOT exposed inside the container — only
# /workspace is bind-mounted, so the same absolute path inside is empty.
if podman run --rm --network=pasta --userns=keep-id \
    --entrypoint /bin/bash \
    -v "$WORKSPACE:/workspace:rw" \
    "$IMAGE_REF" \
    -c "cat $HOST_SENTINEL" 2>/dev/null | grep -q host-secret; then
  echo "FAIL: container could read host path $HOST_SENTINEL" >&2
  exit 1
fi

# 3. Container /etc/passwd is the image's fakeNss, not the host's.
container_passwd=$(podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  "$IMAGE_REF" \
  -c "cat /etc/passwd")
if [[ "$container_passwd" != *wrapix* ]]; then
  echo "FAIL: container /etc/passwd missing image fakeNss wrapix entry" >&2
  echo "$container_passwd" >&2
  exit 1
fi
host_user=$(id -un)
if [[ "$container_passwd" == *"$host_user"* ]] && [[ "$host_user" != "wrapix" ]]; then
  echo "FAIL: container /etc/passwd leaked host user $host_user" >&2
  exit 1
fi

echo "PASS: filesystem-isolation" >&2
