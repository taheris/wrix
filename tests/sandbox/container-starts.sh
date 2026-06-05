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
# shellcheck source=tests/lib/podman-image.sh
source "$SCRIPT_DIR/../lib/podman-image.sh"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); macOS covered by tests/darwin/*"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[[ -e /run/.containerenv ]] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrix-container-start.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF=$(wrix_unique_image_ref "wrix-test-container-starts")
wrix_load_test_image "$IMAGE_STREAM" "wrix-base-claude" "$IMAGE_REF"

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
