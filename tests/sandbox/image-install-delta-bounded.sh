#!/usr/bin/env bash
# Verifier for specs/sandbox.md (image-builder.md Non-Functional #3):
#
#   On Linux, re-installing an image that differs from the cached one
#   in only its top-of-closure layers transfers O(changed-blobs) bytes
#   into the platform store, not O(image-size) bytes.
#
# Strategy: build two test images that share every base-layer blob and
# differ only in their streamLayeredImage customisation layer (perturbed
# via a one-attr claudeConfig change). Install each into an isolated
# podman store and snapshot on-disk size at three points:
#
#   0. empty store
#   1. after install A   -> delta captures the full image footprint
#   2. after install B   -> delta captures only the changed-blob bytes
#
# Assert delta 1->2 is well under delta 0->1. The bound only holds when
# the install transport dedupes against existing blobs (the post-load
# overlay store and skopeo copy --insecure-policy oci-archive: ->
# containers-storage: both satisfy it; a naive walk of the full tar
# would not).
#
#   Linux + rootless podman + nix   -> exercise install path
#   Darwin                          -> exit 77 (Out of Scope, per
#                                       specs/image-builder.md
#                                       "Per-layer-blob-dedup install
#                                       on Darwin")
#   non-Linux non-Darwin            -> exit 77
#   nix or podman missing           -> exit 77

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only; Darwin per-layer-blob-dedup install is Out of Scope (specs/image-builder.md)"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

cd "$REPO_ROOT"

IMAGE_A_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)
IMAGE_B_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base-perturbed)

if [[ "$IMAGE_A_STREAM" = "$IMAGE_B_STREAM" ]]; then
  echo "FAIL: test-image-base and test-image-base-perturbed resolved to the same store path" >&2
  echo "      the perturbation must materialise a distinct image for the delta test to be meaningful" >&2
  exit 1
fi

# Isolated podman store so the test never depends on blobs already in
# the user's store and the user's store never absorbs test bytes.
STORE_ROOT=$(mktemp -d -t wrapix-install-delta.XXXXXX)
cleanup() {
  rm -rf "$STORE_ROOT"
}
trap cleanup EXIT

POD=(podman --root "$STORE_ROOT/root" --runroot "$STORE_ROOT/runroot")
mkdir -p "$STORE_ROOT/root" "$STORE_ROOT/runroot"

snapshot_size() {
  # best-effort stderr suppression: rootless podman lays files under
  # subuid mappings that `du` cannot always stat; we only need the
  # aggregate byte count, not per-file warnings.
  du -sb "$STORE_ROOT/root" 2>/dev/null | awk '{print $1}'
}

SIZE0=$(snapshot_size)

"$IMAGE_A_STREAM" | "${POD[@]}" load -q >/dev/null
SIZE1=$(snapshot_size)

"$IMAGE_B_STREAM" | "${POD[@]}" load -q >/dev/null
SIZE2=$(snapshot_size)

DELTA_A=$((SIZE1 - SIZE0))
DELTA_B=$((SIZE2 - SIZE1))

echo "store size: 0=$SIZE0 1=+A=$SIZE1 2=+B=$SIZE2 (bytes)" >&2
echo "delta A (install A from empty store): $DELTA_A bytes" >&2
echo "delta B (install B with A cached):    $DELTA_B bytes" >&2

if [[ "$DELTA_A" -le 0 ]]; then
  echo "FAIL: installing image A did not grow the store; nothing to compare against" >&2
  exit 1
fi

# Bound: B must be a small fraction of A. The customisation layer is
# tiny (JSON files + the nix-store db registration); even with margin
# for manifest re-emission, overlay metadata, and partial-blob growth,
# B should not approach A. DELTA_A/4 allows up to 4x the changed-blob
# size as overhead and still catches the O(image-size) regression
# cleanly (where DELTA_B ~= DELTA_A).
BOUND=$((DELTA_A / 4))
if [[ "$DELTA_B" -ge "$BOUND" ]]; then
  echo "FAIL: install delta B=$DELTA_B bytes >= bound (delta_A/4)=$BOUND bytes" >&2
  echo "      this indicates the install transport is O(image-size), not O(changed-blobs)" >&2
  echo "      (expected: per-blob dedup against the cached image A's layers)" >&2
  exit 1
fi

echo "PASS: image-install-delta-bounded (delta B=$DELTA_B < bound=$BOUND; image footprint=$DELTA_A)" >&2
