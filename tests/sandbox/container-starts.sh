#!/usr/bin/env bash
# Verifier for criterion 104 of specs/sandbox.md:
#
#   A built sandbox starts a container and exits cleanly on both Linux and
#   macOS.
#
# Runs directly against the host's rootless podman: builds the test sandbox
# image (`.#test-image-base`), pipes it into `podman load`, and asserts the
# container starts (`echo container-started`) and brings up loopback
# networking (`/etc/hosts` contains `localhost`).
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

WORKSPACE=$(mktemp -d -t wrapix-container-start.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  podman rmi -f localhost/wrapix-base:latest >/dev/null 2>&1 || true
}
trap cleanup EXIT

"$IMAGE_STREAM" | podman load >/dev/null
IMAGE_REF="localhost/wrapix-base:latest"

# podman load stores the unqualified manifest tag (`wrapix-base:latest`)
# under a podman-version-dependent ref — sometimes the bare `<name>:<tag>`,
# sometimes `localhost/<name>:<tag>`, sometimes `docker.io/library/<name>:<tag>`
# — depending on the host registries.conf. Re-tag via the loaded image ID
# to $IMAGE_REF so podman run can address it. Same pattern as the launcher
# (lib/util/shell.nix imageLoadStep).
loaded_id=$(podman images --quiet --filter "reference=*wrapix-base*" | head -n1)
[[ -n "$loaded_id" ]] || { echo "FAIL: image not found after podman load" >&2; podman images >&2; exit 1; }
podman tag "$loaded_id" "$IMAGE_REF"

result=$(podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  -w /workspace \
  "$IMAGE_REF" \
  -c "echo container-started")
[[ "$result" == *container-started* ]] || {
  echo "FAIL: container did not start cleanly: $result" >&2
  exit 1
}

result=$(podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  "$IMAGE_REF" \
  -c "cat /etc/hosts | grep localhost")
[[ "$result" == *localhost* ]] || {
  echo "FAIL: loopback not configured in container: $result" >&2
  exit 1
}

echo "PASS: container-start" >&2
