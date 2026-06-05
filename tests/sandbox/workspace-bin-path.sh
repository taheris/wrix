#!/usr/bin/env bash
# Verifier for the /workspace/bin PATH-prepend criteria of specs/sandbox.md
# (FR12, "Workspace bin/ PATH prepend"):
#
#   - When /workspace/bin exists, it appears first on PATH, so a
#     consumer-supplied shim at /workspace/bin/<name> resolves ahead of a
#     same-named binary baked into the image.
#   - When /workspace/bin does not exist, the container's PATH does not
#     contain /workspace/bin.
#
# Exercises the real container entrypoint (/entrypoint.sh) rather than
# grepping the source: the entrypoint runs the command override after the
# PATH prepend, so we observe the runtime PATH the agent would see.
#
#   Linux + rootless podman + nix  -> exercise the image
#   Darwin                         -> exit 77 (macOS path covered by tests/darwin/*)
#   non-Linux non-Darwin           -> exit 77
#   nix or podman missing          -> exit 77

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=../lib/podman-image.sh
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

WORKSPACE=$(mktemp -d -t wrapix-workspace-bin.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF="localhost/wrapix-test-workspace-bin-path:latest"
# Clear stale wrapix-base images BEFORE load so the post-load retag has exactly
# one candidate (same retag pattern as container-starts.sh / shell.nix).
wrapix_remove_test_image_refs "wrapix-base-claude" "$IMAGE_REF"
"$IMAGE_STREAM" | podman load >/dev/null
wrapix_tag_loaded_image_id "wrapix-base-claude" "$IMAGE_REF"

run_entrypoint() {
  # Invoke the default entrypoint (/entrypoint.sh) with a command override so
  # the line that prepends /workspace/bin runs before our probe.
  podman run --rm --network=pasta --userns=keep-id \
    -v "$WORKSPACE:/workspace:rw" \
    "$IMAGE_REF" \
    bash -c "$1"
}

# --- Case 1: shim present -> /workspace/bin wins over the baked binary --------
mkdir -p "$WORKSPACE/bin"
SHIM_SENTINEL="WRAPIX_SHIM_GIT_WINS"
cat > "$WORKSPACE/bin/git" <<EOF
#!/bin/bash
echo "$SHIM_SENTINEL"
EOF
chmod +x "$WORKSPACE/bin/git"

present_path=$(run_entrypoint 'printf "%s" "$PATH"')
case ":$present_path:" in
  ":/workspace/bin:"*) ;;
  *) echo "FAIL: /workspace/bin not first on PATH when present: $present_path" >&2; exit 1 ;;
esac

resolved=$(run_entrypoint 'command -v git')
[[ "$resolved" = "/workspace/bin/git" ]] || {
  echo "FAIL: git did not resolve to the shim: $resolved" >&2
  exit 1
}

shim_out=$(run_entrypoint 'git')
[[ "$shim_out" = "$SHIM_SENTINEL" ]] || {
  echo "FAIL: shim did not execute (got: $shim_out)" >&2
  exit 1
}

# --- Case 2: dir absent -> PATH unchanged, baked binary still resolves --------
rm -rf "${WORKSPACE:?}/bin"

absent_path=$(run_entrypoint 'printf "%s" "$PATH"')
case ":$absent_path:" in
  *":/workspace/bin:"*) echo "FAIL: /workspace/bin on PATH when dir absent: $absent_path" >&2; exit 1 ;;
  *) ;;
esac

baked_git=$(run_entrypoint 'command -v git')
[[ -n "$baked_git" && "$baked_git" != /workspace/bin/* ]] || {
  echo "FAIL: baked git not resolvable without the shim (got: $baked_git)" >&2
  exit 1
}

echo "PASS: workspace-bin-path (shim wins when present; PATH unchanged when absent)" >&2
