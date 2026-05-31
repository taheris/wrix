#!/usr/bin/env bash
# Verifier for criterion 106 of specs/sandbox.md:
#
#   Files created inside /workspace carry the host UID/GID, not a
#   container-internal UID.
#
# Runs directly against the host's rootless podman with `--userns=keep-id`:
# the host caller's UID maps to the same UID inside the container's user
# namespace, so a write from container-side root or wrapix lands on disk
# owned by the host caller.
#
#   Linux + rootless podman + nix  -> exercise the image
#   Darwin                         -> exit 77 (Darwin path: tests/darwin/uid.nix)
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
case "$uname_s" in
  Linux) ;;
  Darwin) skip "Darwin path verified by .#checks.<sys>.darwin-uid-integration" ;;
  *) skip "unsupported platform: $uname_s" ;;
esac
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-uid-mapping.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  podman rmi -f localhost/wrapix-base:latest >/dev/null 2>&1 || true
}
trap cleanup EXIT

"$IMAGE_STREAM" | podman load >/dev/null
IMAGE_REF="localhost/wrapix-base:latest"
HOST_UID=$(id -u)

# Write a file from inside the container.
podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  -w /workspace \
  "$IMAGE_REF" \
  -c "echo created-in-container > /workspace/container-file.txt"

[[ -f "$WORKSPACE/container-file.txt" ]] || {
  echo "FAIL: container-file.txt missing on host after container write" >&2
  exit 1
}
got_uid=$(stat -c '%u' "$WORKSPACE/container-file.txt")
[[ "$got_uid" = "$HOST_UID" ]] || {
  echo "FAIL: container-file.txt has UID $got_uid, expected $HOST_UID" >&2
  exit 1
}
content=$(cat "$WORKSPACE/container-file.txt")
[[ "$content" == *created-in-container* ]] || {
  echo "FAIL: container-file.txt content mismatch: $content" >&2
  exit 1
}

# Create a subdirectory + nested file; ownership should propagate.
podman run --rm --network=pasta --userns=keep-id \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  -w /workspace \
  "$IMAGE_REF" \
  -c "mkdir -p /workspace/subdir && echo nested > /workspace/subdir/nested.txt"

got_dir_uid=$(stat -c '%u' "$WORKSPACE/subdir")
[[ "$got_dir_uid" = "$HOST_UID" ]] || {
  echo "FAIL: subdir has UID $got_dir_uid, expected $HOST_UID" >&2
  exit 1
}
got_nested_uid=$(stat -c '%u' "$WORKSPACE/subdir/nested.txt")
[[ "$got_nested_uid" = "$HOST_UID" ]] || {
  echo "FAIL: subdir/nested.txt has UID $got_nested_uid, expected $HOST_UID" >&2
  exit 1
}

echo "PASS: uid-mapping" >&2
