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

IMAGE_REF="localhost/wrapix-base:latest"
# Clear stale wrapix-base images BEFORE load so the post-load retag has
# exactly one candidate to pick. `podman load` of a streamLayeredImage
# tarball stores the image under the manifest tag with a podman-version-
# dependent normalization (`wrapix-base:latest`, `localhost/wrapix-base:latest`,
# or `docker.io/library/wrapix-base:latest`), so we re-tag via the loaded
# ID to $IMAGE_REF. If a previous run left a stale `wrapix-base` around,
# the `head -n1` pick is non-deterministic and we'd risk tagging the
# stale ID to the new ref — and silently exercising the old image whose
# config may lack the env vars (e.g. WRAPIX_PREK_HOOKS) the entrypoint
# depends on. Same retag pattern as lib/util/shell.nix imageLoadStep.
podman images --quiet --filter "reference=*wrapix-base*" | xargs -r podman rmi -f >/dev/null 2>&1 || true
"$IMAGE_STREAM" | podman load >/dev/null
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
