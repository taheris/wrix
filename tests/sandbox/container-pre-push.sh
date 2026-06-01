#!/usr/bin/env bash
# Verifier for criterion 154 of specs/pre-commit.md:
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

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s)"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
command -v git    >/dev/null 2>&1 || skip "git not on PATH"
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[ -e /run/.containerenv ] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-container-pre-push.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists localhost/wrapix-base:latest; then
    podman rmi localhost/wrapix-base:latest >/dev/null
  fi
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
if podman image exists "$IMAGE_REF"; then
  podman rmi "$IMAGE_REF" >/dev/null
fi
"$IMAGE_STREAM" | podman load >/dev/null
loaded_id=$(podman images --quiet --filter "reference=*wrapix-base*" | head -n1)
[[ -n "$loaded_id" ]] || { echo "FAIL: image not found after podman load" >&2; podman images >&2; exit 1; }
podman tag "$loaded_id" "$IMAGE_REF"

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
touch /workspace/.git/sentinel-fired-prepush
SCRIPT
chmod 755 "$WORKSPACE/.git/sentinel-pre-push.sh"

git -C "$WORKSPACE" add -A
git -C "$WORKSPACE" commit -q -m initial

# Push from inside the container. The entrypoint installs
# core.hooksPath -> prek bundle, then `git push` triggers the pre-push
# shim which runs prek hook-impl --hook-type=pre-push and invokes the
# sentinel before the actual push proceeds.
podman run --rm --network=pasta --userns=keep-id \
  -e HOME=/home/wrapix \
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
