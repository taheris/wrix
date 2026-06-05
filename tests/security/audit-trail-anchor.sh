#!/usr/bin/env bash
# Verifier for criterion 207 of specs/security.md:
#
#   After a sandbox session, a session-metadata index file exists under
#   /workspace/.wrapix/log/; its timestamp_start, timestamp_end,
#   exit_code, mode, and claude_session_dir fields are populated; and
#   claude_session_dir resolves to an existing directory.
#
# Runs the entrypoint with `/bin/true` as the command override so the
# EXIT trap fires without booting any agent runtime. Asserts the
# host-side $WORKSPACE/.wrapix/log/*.json carries the contract fields.

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
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); macOS entrypoint covered by tests/darwin/*"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
command -v jq     >/dev/null 2>&1 || skip "jq not on PATH"
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[ -e /run/.containerenv ] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-audit-trail.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF="localhost/wrapix-test-audit-trail-anchor:latest"
# Clear stale wrapix-base images BEFORE load so the post-load retag has
# exactly one candidate to pick. `podman load` of a streamLayeredImage
# tarball stores the image under the manifest tag with a podman-version-
# dependent normalization (`wrapix-base-claude:latest`, `localhost/wrapix-base-claude:latest`,
# or `docker.io/library/wrapix-base-claude:latest`), so we re-tag via the loaded
# ID to $IMAGE_REF. If a previous run left a stale `wrapix-base` around,
# the `head -n1` pick is non-deterministic and we'd risk tagging the
# stale ID to the new ref — and silently exercising the old image whose
# config may lack the env vars (e.g. WRAPIX_PREK_HOOKS) the entrypoint
# depends on. Same retag pattern as lib/util/shell.nix imageLoadStep.
wrapix_remove_test_image_refs "wrapix-base-claude" "$IMAGE_REF"
"$IMAGE_STREAM" | podman load >/dev/null
wrapix_tag_loaded_image_id "wrapix-base-claude" "$IMAGE_REF"

# Run the entrypoint with a no-op command override. The launcher's
# always-on env (HOME, GIT_AUTHOR_*, GIT_COMMITTER_*) is replicated so
# the entrypoint's claude-config branch and write_session_log fire the
# same way they would under real `wrapix run`.
podman run --rm --network=pasta --userns=keep-id \
  -e HOME=/home/wrapix \
  -e WRAPIX_AGENT=claude \
  -e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com \
  -e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com \
  -v "$WORKSPACE:/workspace:rw" \
  "$IMAGE_REF" /bin/true

log_count=$(find "$WORKSPACE/.wrapix/log" -maxdepth 1 -name '*.json' 2>/dev/null | wc -l)
[[ "$log_count" -eq 1 ]] || {
  echo "FAIL: expected exactly one session-metadata JSON, got $log_count" >&2
  exit 1
}

log_file=$(find "$WORKSPACE/.wrapix/log" -maxdepth 1 -name '*.json' | head -n1)

for field in timestamp_start timestamp_end exit_code mode claude_session_dir; do
  value=$(jq -r ".${field} // empty" "$log_file")
  [[ -n "$value" ]] || {
    echo "FAIL: field $field empty/null in $log_file" >&2
    cat "$log_file" >&2
    exit 1
  }
done

claude_dir=$(jq -r '.claude_session_dir' "$log_file")
[[ "$claude_dir" = "/workspace/.claude" ]] || {
  echo "FAIL: unexpected claude_session_dir: $claude_dir" >&2
  exit 1
}
[[ -d "$WORKSPACE/.claude" ]] || {
  echo "FAIL: claude_session_dir does not exist on host: $WORKSPACE/.claude" >&2
  exit 1
}

echo "PASS: audit-trail-anchor" >&2
