#!/usr/bin/env bash
# Verifier for specs/pre-commit.md § Hook Installation in Profile Containers:
#
#   A pre-push hook configured in `.pre-commit-config.yaml` fires when
#   `git push` runs inside a profile container.
#
# Seeds a workspace with a git repo + a local bare remote + a
# .pre-commit-config.yaml naming two pre-push hooks (a skip-if-missing
# wrapper probe and a sentinel touch), commits a seed file on the host
# (no hooks installed there yet), then runs `git push origin main`
# inside the container. The entrypoint installs core.hooksPath, the
# pre-push shim invokes prek which dispatches the configured hooks, the
# sentinel touch proves the chain fired, and the push reaches the bare
# remote.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/podman-image.sh
source "$SCRIPT_DIR/../lib/podman-image.sh"

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    fail "$command_name not on PATH"
  fi
}

uname_s="$(uname -s)"
[[ "$uname_s" == "Linux" ]] || fail "Linux-only verifier (uname=$uname_s)"
require_command git
require_command nix
require_command podman
require_command skopeo

PODMAN_BIN="$(command -v podman)"
SKOPEO_BIN="$(command -v skopeo)"
NESTED_RUNTIME_DIR=""
NESTED_UID=""
NESTED_GID=""
NESTED_PODMAN_READY=0
PODMAN_COMMAND=("$PODMAN_BIN")
PODMAN_RUN_OPTIONS=(--network=pasta --cap-add=NET_ADMIN)
WORKSPACE=""
IMAGE_REF=""
CONTAINER_NAME="wrix-test-container-pre-push-$$"

podman() {
  "${PODMAN_COMMAND[@]}" "$@"
}

skopeo() {
  if [[ -z "$NESTED_RUNTIME_DIR" ]]; then
    "$SKOPEO_BIN" "$@"
    return
  fi

  setpriv --reuid="$NESTED_UID" --regid="$NESTED_GID" --clear-groups \
    env CONTAINERS_STORAGE_CONF="$NESTED_RUNTIME_DIR/storage.conf" \
    HOME="$NESTED_RUNTIME_DIR/home" TMPDIR="$NESTED_RUNTIME_DIR/tmp" \
    "$SKOPEO_BIN" "$@"
}

cleanup() {
  local status="$?"
  trap - EXIT
  if [[ -n "$IMAGE_REF" ]] && podman container exists "$CONTAINER_NAME"; then
    if ! podman rm --force "$CONTAINER_NAME" >/dev/null; then
      printf 'WARN: could not remove test container %s\n' "$CONTAINER_NAME" >&2
    fi
  fi
  if [[ -n "$IMAGE_REF" ]] && podman image exists "$IMAGE_REF"; then
    if ! podman rmi "$IMAGE_REF" >/dev/null; then
      printf 'WARN: could not remove test image %s\n' "$IMAGE_REF" >&2
    fi
  fi
  if [[ "$NESTED_PODMAN_READY" -eq 1 ]]; then
    if ! podman system migrate; then
      printf 'WARN: could not stop nested Podman user namespace\n' >&2
    fi
  fi
  if [[ -n "$WORKSPACE" ]]; then
    rm -rf "$WORKSPACE"
  fi
  if [[ -n "$NESTED_RUNTIME_DIR" ]]; then
    rm -rf "$NESTED_RUNTIME_DIR"
  fi
  exit "$status"
}
trap cleanup EXIT

if [[ -e /run/.containerenv ]]; then
  require_command setpriv
  if ! NESTED_UID="$(id -u wrix)"; then
    fail "nested Podman requires the wrix runtime user"
  fi
  if ! NESTED_GID="$(id -g wrix)"; then
    fail "nested Podman requires the wrix runtime group"
  fi
  NESTED_RUNTIME_DIR="$(mktemp -d -t wrix-pre-push-podman.XXXXXX)"
  mkdir -p "$NESTED_RUNTIME_DIR/home/.config/containers" "$NESTED_RUNTIME_DIR/tmp"
  cat >"$NESTED_RUNTIME_DIR/storage.conf" <<EOF
[storage]
driver = "vfs"
runroot = "$NESTED_RUNTIME_DIR/runroot"
graphroot = "$NESTED_RUNTIME_DIR/graphroot"
EOF
  printf '%s\n' '{"default":[{"type":"insecureAcceptAnything"}]}' \
    >"$NESTED_RUNTIME_DIR/home/.config/containers/policy.json"
  chown -R "$NESTED_UID:$NESTED_GID" "$NESTED_RUNTIME_DIR"
  PODMAN_COMMAND=(
    setpriv --reuid="$NESTED_UID" --regid="$NESTED_GID" --clear-groups
    env CONTAINERS_STORAGE_CONF="$NESTED_RUNTIME_DIR/storage.conf"
    HOME="$NESTED_RUNTIME_DIR/home" TMPDIR="$NESTED_RUNTIME_DIR/tmp"
    "$PODMAN_BIN"
  )
  # Nested Podman cannot use the outer container's absent TUN device; hooks need no egress.
  PODMAN_RUN_OPTIONS=(--network=none --cap-add=NET_ADMIN --pid=host --ipc=host --uts=host)
  NESTED_PODMAN_READY=1
fi

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base-direct)
WORKSPACE=$(mktemp -d -t wrix-container-pre-push.XXXXXX)
IMAGE_REF=$(wrix_unique_image_ref "wrix-test-container-pre-push")
wrix_load_test_image "$IMAGE_STREAM" "wrix-base" "$IMAGE_REF"

# Seed the workspace: working repo + bare file:// remote + sentinel +
# initial commit (made on the host; no hooks fire because core.hooksPath
# is unset until the container's entrypoint installs it).
git -C "$WORKSPACE" init -q -b main
git -C "$WORKSPACE" config user.email test@example.com
git -C "$WORKSPACE" config user.name Test
git -C "$WORKSPACE" init --bare -q remote.git
git -C "$WORKSPACE" remote add origin "file:///workspace/remote.git"
echo seed > "$WORKSPACE/seed.txt"

cat > "$WORKSPACE/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: wrapper-on-path
        name: wrapper-on-path
        entry: skip-if-missing nonexistent-tool-xyz -- false
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
      - id: sentinel
        name: sentinel
        entry: /workspace/.git/sentinel-pre-push.sh
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML

cat > "$WORKSPACE/.git/sentinel-pre-push.sh" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
touch /workspace/.git/sentinel-fired-prepush
SCRIPT
chmod 755 "$WORKSPACE/.git/sentinel-pre-push.sh"

git -C "$WORKSPACE" add -A
git -C "$WORKSPACE" commit -q -m initial

if [[ -n "$NESTED_RUNTIME_DIR" ]]; then
  chown -R "$NESTED_UID:$NESTED_GID" "$WORKSPACE"
fi

# Push from inside the container. The entrypoint installs
# core.hooksPath -> prek bundle, then `git push` triggers the pre-push
# shim which runs prek hook-impl --hook-type=pre-push and invokes the
# sentinel before the actual push proceeds.
podman run "${PODMAN_RUN_OPTIONS[@]}" --rm --name "$CONTAINER_NAME" \
  -e HOME=/home/wrix \
  -e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com \
  -e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com \
  -v "$WORKSPACE:/workspace:rw" \
  "$IMAGE_REF" \
  /bin/bash -c "cd /workspace && git push origin main"

# Sentinel side-effect proves the pre-push hook chain fired.
[[ -f "$WORKSPACE/.git/sentinel-fired-prepush" ]] || {
  echo "FAIL: pre-push sentinel did not fire" >&2
  exit 1
}

# The push reached the bare remote (HEAD ref exists).
git -C "$WORKSPACE/remote.git" rev-parse refs/heads/main >/dev/null || {
  echo "FAIL: push did not land on the bare remote" >&2
  exit 1
}

echo "PASS: container-pre-push" >&2
