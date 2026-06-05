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
# the install transport dedupes against existing blobs by digest. This
# drives the launcher's real transport — skopeo copy oci-archive: ->
# containers-storage: (lib/util/shell.nix imageLoadStep) — not podman
# load, so the promoted criterion exercises the same binary/argv as
# production. containers-storage hashes each blob on write and skips
# ones already present; a naive walk of the full tar would not.
#
#   Linux + rootless podman + skopeo + nix -> exercise install path
#   Darwin                          -> exit 77 (Out of Scope, per
#                                       specs/image-builder.md
#                                       "Per-layer-blob-dedup install
#                                       on Darwin")
#   non-Linux non-Darwin            -> exit 77
#   nix / podman / skopeo missing   -> exit 77

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
command -v skopeo >/dev/null 2>&1 || skip "skopeo not on PATH"
# Nested rootless podman deadlocks on overlay-backed `skopeo copy ...
# containers-storage:`; podman is present but non-functional, so bail
# cleanly rather than hang the gate. Matches the guard on the other
# image-load verifiers (image-install-no-rewrite.sh).
[ -e /run/.containerenv ] && skip "nested container: podman storage unavailable"

cd "$REPO_ROOT"

IMAGE_A_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)
IMAGE_B_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base-perturbed)

if [[ "$IMAGE_A_STREAM" = "$IMAGE_B_STREAM" ]]; then
  echo "FAIL: test-image-base and test-image-base-perturbed resolved to the same store path" >&2
  echo "      the perturbation must materialise a distinct image for the delta test to be meaningful" >&2
  exit 1
fi

# Isolated podman store so the test never depends on blobs already in
# the user's store and the user's store never absorbs test bytes. Both
# podman and skopeo's containers-storage transport honor
# CONTAINERS_STORAGE_CONF, so every byte the install path writes lands
# here.
STORE_ROOT=$(mktemp -d -t wrix-install-delta.XXXXXX)
cleanup() {
  # rootless podman writes the overlay store under mapped subuids the
  # caller can't unlink directly; reclaim it from inside the user
  # namespace, falling back to a plain rm. A leftover temp dir is
  # non-fatal, so warn rather than fail if neither works. Restore the
  # body's exit status afterwards: a bare EXIT trap adopts its last
  # command's status, so a cleanup failure would otherwise flip a
  # passing run to a failure.
  local rc=$?
  if ! podman unshare rm -rf "$STORE_ROOT"; then
    rm -rf "$STORE_ROOT" \
      || echo "WARN: could not remove temp store $STORE_ROOT" >&2
  fi
  exit "$rc"
}
trap cleanup EXIT

mkdir -p "$STORE_ROOT/root" "$STORE_ROOT/runroot" "$STORE_ROOT/work"

cat >"$STORE_ROOT/storage.conf" <<EOF
[storage]
driver = "overlay"
graphroot = "$STORE_ROOT/root"
runroot = "$STORE_ROOT/runroot"
EOF
export CONTAINERS_STORAGE_CONF="$STORE_ROOT/storage.conf"

# Inline mirror of lib/util/shell.nix imageLoadStep: docker-archive ->
# oci-archive -> containers-storage, the launcher's real install
# transport. containers-storage dedupes blobs by digest on write, so
# installing B after A only transfers B's changed customisation layer.
# Kept narrow so divergence from the launcher snippet is visible at
# review time. A and B use distinct refs; dedup is by blob content, not
# by ref, so the shared base layers are skipped on B's install.
install_image() {
  local stream="$1" ref="$2"
  local stage="$STORE_ROOT/work/stage"
  rm -rf "$stage"
  mkdir -p "$stage"
  "$stream" >"$stage/image.tar"
  skopeo --insecure-policy copy --quiet \
    "docker-archive:$stage/image.tar" \
    "oci-archive:$stage/image.oci"
  skopeo --insecure-policy copy --quiet \
    "oci-archive:$stage/image.oci" \
    "containers-storage:$ref"
  rm -rf "$stage"
}

snapshot_size() {
  # best-effort stderr suppression: rootless overlay lays files under
  # subuid mappings that `du` cannot always stat; we only need the
  # aggregate byte count, not per-file warnings.
  du -sb "$STORE_ROOT/root" 2>/dev/null | awk '{print $1}'
}

SIZE0=$(snapshot_size)

install_image "$IMAGE_A_STREAM" "localhost/wrix-delta-a:test"
SIZE1=$(snapshot_size)

install_image "$IMAGE_B_STREAM" "localhost/wrix-delta-b:test"
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
