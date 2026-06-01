#!/usr/bin/env bash
# Verifier for specs/sandbox.md (image install path):
#
#   A second spawn of an already-loaded image performs no writes to the
#   platform store's layer directory (measurable via store size or
#   per-blob mtime).
#
# Strategy: drive the launcher's image-install path twice against an
# isolated podman store using the same test image, and snapshot the
# layer directory between invocations. The second install must produce
# a zero size delta and leave every per-blob mtime untouched.
#
# Either contract layer is sufficient for the property to hold:
#   - the launcher's digest-preflight short-circuit (when the image's
#     content digest is already present, no install transport runs); or
#   - the skopeo install transport being properly content-addressable
#     (containers-storage dedupes blobs by digest on write).
# The test passes as long as at least one of them holds, which is what
# specs/sandbox.md § Image install path requires.
#
#   Linux + rootless podman + skopeo + nix  -> exercise install path
#   Darwin                                  -> exit 77 (per-blob-dedup
#                                              install is Out of Scope;
#                                              see specs/image-builder.md)
#   non-Linux non-Darwin                    -> exit 77
#   nested container                        -> exit 77 (rootless podman
#                                              load deadlocks on overlay)
#   nix / podman / skopeo / jq / tar missing -> exit 77

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
command -v jq     >/dev/null 2>&1 || skip "jq not on PATH"
command -v tar    >/dev/null 2>&1 || skip "tar not on PATH"
# Nested rootless podman deadlocks on overlay-backed `podman load` /
# `skopeo copy ... containers-storage:` flows; matches the guard on the
# other image-load verifiers (image-install-delta-bounded.sh).
[[ -e /run/.containerenv ]] && skip "nested container: podman storage unavailable"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

# Isolated podman store. Both podman and skopeo's containers-storage
# transport honor CONTAINERS_STORAGE_CONF, so every byte the install
# path writes lands here, never in the user's real store.
STORE_ROOT=$(mktemp -d -t wrapix-norewrite.XXXXXX)
cleanup() {
  rm -rf "$STORE_ROOT"
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

# Stream once; reuse the materialised tar for both invocations.
"$IMAGE_STREAM" >"$STORE_ROOT/image.tar"

# Mirror lib/sandbox/image.nix digestFile: the OCI config blob hash
# (== podman's `.Id`). The launcher consumes the same value from
# IMAGE_DIGEST_PATH for its preflight.
DESIRED_DIGEST=$(tar -xOf "$STORE_ROOT/image.tar" manifest.json \
  | jq -r '.[0].Config' \
  | sed 's/\.json$//')
if [[ -z "$DESIRED_DIGEST" ]]; then
  echo "FAIL: could not extract content digest from streamed image" >&2
  exit 1
fi

IMAGE_REF="localhost/wrapix-norewrite:test"

# Inline mirror of lib/util/shell.nix imageLoadStep — digest preflight,
# then docker-archive -> oci-archive -> containers-storage. Kept narrow
# so divergence from the launcher snippet is visible at review time.
run_install() {
  local stage="$STORE_ROOT/work/stage"
  rm -rf "$stage"
  mkdir -p "$stage"

  if podman image inspect --format '{{.Id}}' "$DESIRED_DIGEST" >/dev/null 2>&1; then
    # best-effort: re-aliasing the same ref to the same image Id is
    # benign; the byte-level no-op is what the assertion below checks.
    podman tag "$DESIRED_DIGEST" "$IMAGE_REF" >/dev/null 2>&1 || true
    return 0
  fi

  skopeo --insecure-policy copy --quiet \
    "docker-archive:$STORE_ROOT/image.tar" \
    "oci-archive:$stage/image.oci"
  skopeo --insecure-policy copy --quiet \
    "oci-archive:$stage/image.oci" \
    "containers-storage:$IMAGE_REF"
  rm -rf "$stage"
}

snapshot_size() {
  # rootless podman lays files under subuid mappings that `du` cannot
  # always stat; only the aggregate byte count matters here.
  du -sb "$STORE_ROOT/root" 2>/dev/null | awk '{print $1}'
}

snapshot_blobs() {
  # Layer blobs and their bookkeeping live under overlay/ (rootfs diff
  # dirs) and overlay-layers/ (metadata). Restricting the snapshot to
  # those keeps podman's transient files (boltdb, storage lock,
  # tmp/run state) from false-failing the "no writes to the layer
  # directory" assertion. %T@ is mtime, %s is size — together they
  # detect both rewrites with the same byte count and pure metadata
  # touches.
  find "$STORE_ROOT/root/overlay" "$STORE_ROOT/root/overlay-layers" \
    -type f -printf '%P %T@ %s\n' 2>/dev/null | sort
}

run_install
SIZE1=$(snapshot_size)
BLOBS_BEFORE="$STORE_ROOT/blobs.before"
snapshot_blobs >"$BLOBS_BEFORE"

run_install
SIZE2=$(snapshot_size)
BLOBS_AFTER="$STORE_ROOT/blobs.after"
snapshot_blobs >"$BLOBS_AFTER"

DELTA=$((SIZE2 - SIZE1))

echo "store size after invocation 1: $SIZE1 bytes" >&2
echo "store size after invocation 2: $SIZE2 bytes (delta=$DELTA)" >&2

if [[ "$DELTA" -ne 0 ]]; then
  echo "FAIL: second invocation grew the store by $DELTA bytes (expected 0)" >&2
  echo "      neither the digest preflight nor the per-blob-dedup transport short-circuited the install" >&2
  exit 1
fi

if ! diff -q "$BLOBS_BEFORE" "$BLOBS_AFTER" >/dev/null 2>&1; then
  echo "FAIL: second invocation rewrote blobs or touched mtimes under the layer directory:" >&2
  diff -u "$BLOBS_BEFORE" "$BLOBS_AFTER" | head -40 >&2
  exit 1
fi

echo "PASS: image-install-no-rewrite (layer directory unchanged across two installs of the same image)" >&2
