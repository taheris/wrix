#!/usr/bin/env bash
# Verifier for the entrypoint agent-binary guard criterion of
# specs/sandbox.md § Entrypoint binary guard (FR8, "Agent runtime axis"):
#
#   Before exec'ing the agent selected by WRAPIX_AGENT, the entrypoint
#   verifies the named binary is present (command -v) and hard-errors
#   with a clear message naming the missing agent when it is absent
#   (e.g. WRAPIX_AGENT=pi against a claude image on the raw-launcher
#   path), instead of dead-ending at a bare 'command not found'.
#
# Exercises the real container entrypoint (/entrypoint.sh) rather than
# grepping source: the guard runs only on agent-exec runs ($# -eq 0),
# so each case invokes the default entrypoint with no command override.
#
#   - Guard fires: WRAPIX_AGENT=claude against test-image-base (which bakes
#     `hello` as the agent stand-in — no claude, no loom-direct-runner)
#     hard-errors non-zero, naming the agent.
#   - Guard passes: WRAPIX_AGENT=direct with a /workspace/bin shim named
#     loom-direct-runner runs the shim (its sentinel reaches stdout)
#     rather than blocking.
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

WORKSPACE=$(mktemp -d -t wrapix-agent-binary-guard.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF="localhost/wrapix-test-agent-binary-guard:latest"
# Clear stale wrapix-base images BEFORE load so the post-load retag has exactly
# one candidate (same retag pattern as container-starts.sh / workspace-bin-path.sh).
wrapix_remove_test_image_refs "wrapix-base-claude" "$IMAGE_REF"
"$IMAGE_STREAM" | podman load >/dev/null
wrapix_tag_loaded_image_id "wrapix-base-claude" "$IMAGE_REF"

# Run the default entrypoint with no command override ($# -eq 0 inside the
# container) so the agent-exec path — and thus the guard — is reached.
run_agent() {
  podman run --rm --network=pasta --userns=keep-id \
    -e "WRAPIX_AGENT=$1" \
    -v "$WORKSPACE:/workspace:rw" \
    "$IMAGE_REF"
}

# --- Case 1: selected agent absent -> guard fires, names the agent ------------
rm -rf "${WORKSPACE:?}/bin"
set +e
fire_out=$(run_agent claude 2>&1)
fire_rc=$?
set -e
[[ "$fire_rc" -ne 0 ]] || {
  echo "FAIL: WRAPIX_AGENT=claude against an image without claude exited 0 (guard did not fire)" >&2
  echo "  output: $fire_out" >&2
  exit 1
}
case "$fire_out" in
  *"WRAPIX_AGENT=claude"*"not present"*) ;;
  *)
    echo "FAIL: guard error did not name the missing agent clearly: $fire_out" >&2
    exit 1
    ;;
esac
case "$fire_out" in
  *"command not found"*)
    echo "FAIL: guard dead-ended at a bare 'command not found': $fire_out" >&2
    exit 1
    ;;
  *) ;;
esac

# --- Case 2: selected agent present -> guard passes, agent runs ---------------
mkdir -p "$WORKSPACE/bin"
SENTINEL="WRAPIX_DIRECT_RAN_OK"
cat > "$WORKSPACE/bin/loom-direct-runner" <<EOF
#!/bin/bash
echo "$SENTINEL"
EOF
chmod +x "$WORKSPACE/bin/loom-direct-runner"

set +e
pass_out=$(run_agent direct 2>&1)
pass_rc=$?
set -e
[[ "$pass_rc" -eq 0 ]] || {
  echo "FAIL: WRAPIX_AGENT=direct with a present binary exited $pass_rc (guard wrongly fired)" >&2
  echo "  output: $pass_out" >&2
  exit 1
}
case "$pass_out" in
  *"$SENTINEL"*) ;;
  *)
    echo "FAIL: present agent did not run past the guard (sentinel missing): $pass_out" >&2
    exit 1
    ;;
esac

echo "PASS: agent-binary-guard (fires when binary absent; passes when present)" >&2
