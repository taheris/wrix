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

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); macOS entrypoint covered by tests/darwin/*"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
command -v jq     >/dev/null 2>&1 || skip "jq not on PATH"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)

WORKSPACE=$(mktemp -d -t wrapix-audit-trail.XXXXXX)
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

# Run the entrypoint with a no-op command override. The launcher's
# always-on env (HOME, GIT_AUTHOR_*, GIT_COMMITTER_*) is replicated so
# the entrypoint's claude-config branch and write_session_log fire the
# same way they would under real `wrapix run`.
podman run --rm --network=pasta --userns=keep-id \
  -e HOME=/home/wrapix \
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
