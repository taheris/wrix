#!/usr/bin/env bash
# Verifier for criterion 152 of specs/pre-commit.md:
#
#   A pre-commit hook configured in `.pre-commit-config.yaml` fires when
#   `git commit` runs inside a profile container.
#
# Seeds a workspace with a git repo + a .pre-commit-config.yaml naming
# two hooks (a skip-if-missing wrapper probe and a sentinel touch), then
# runs `git add -A && git commit -m test` inside the container with the
# entrypoint as the entrypoint. The entrypoint installs core.hooksPath
# to wrapix.prekHooks before exec'ing the override; the commit's
# pre-commit shim then dispatches the .pre-commit-config.yaml hooks via
# prek. Sentinel marker file proves the chain fired.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s)"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
command -v git    >/dev/null 2>&1 || skip "git not on PATH"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-container-pre-commit.XXXXXX)
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

# Seed the workspace: git repo + .pre-commit-config.yaml + sentinel.
git -C "$WORKSPACE" init -q -b main
git -C "$WORKSPACE" config user.email test@example.com
git -C "$WORKSPACE" config user.name Test
echo seed > "$WORKSPACE/seed.txt"

cat > "$WORKSPACE/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: wrapper-on-path
        name: wrapper-on-path
        entry: skip-if-missing nonexistent-tool-xyz -- false
        language: system
        stages: [pre-commit]
        always_run: true
        pass_filenames: false
      - id: sentinel
        name: sentinel
        entry: /workspace/.git/sentinel-pre-commit.sh
        language: system
        stages: [pre-commit]
        always_run: true
        pass_filenames: false
YAML

cat > "$WORKSPACE/.git/sentinel-pre-commit.sh" <<'SCRIPT'
#!/usr/bin/env bash
touch /workspace/.git/sentinel-fired-precommit
SCRIPT
chmod 755 "$WORKSPACE/.git/sentinel-pre-commit.sh"

# Run the container with `git add -A && git commit` as the override.
podman run --rm --network=pasta --userns=keep-id \
  -e HOME=/home/wrapix \
  -e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com \
  -e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com \
  -v "$WORKSPACE:/workspace:rw" \
  "$IMAGE_REF" \
  /bin/bash -c "cd /workspace && git add -A && git commit -m test"

# Sentinel side-effect proves the hook chain fired via core.hooksPath.
[[ -f "$WORKSPACE/.git/sentinel-fired-precommit" ]] || {
  echo "FAIL: pre-commit sentinel did not fire" >&2
  exit 1
}

# The wrapper-on-path probe would have failed the commit if
# `skip-if-missing` were not on PATH (`command not found` is non-zero);
# the commit succeeding above is the evidence that both wrappers landed.

echo "PASS: container-pre-commit" >&2
