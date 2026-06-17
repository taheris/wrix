#!/usr/bin/env bash
# Verifier for the entrypoint agent-binary guard criterion of
# specs/sandbox.md § Entrypoint binary guard (FR8, "Agent runtime axis"):
#
#   Before exec'ing the agent selected by WRIX_AGENT, the entrypoint
#   verifies the named binary is present (command -v) and hard-errors
#   with a clear message naming the missing agent when it is absent
#   (e.g. WRIX_AGENT=pi against a claude image on the raw-launcher
#   path), instead of dead-ending at a bare 'command not found'.
#
# Exercises the real container entrypoint (/entrypoint.sh) rather than
# grepping source: the guard runs only on agent-exec runs ($# -eq 0),
# so each case invokes the default entrypoint with no command override.
#
#   - Mismatch guard fires: runtime WRIX_AGENT disagrees with the image's baked
#     /etc/wrix/image-agent metadata, naming the ProfileConfig/image variant
#     problem.
#   - Binary guard fires: WRIX_AGENT=claude against test-image-base (which bakes
#     `hello` as the agent stand-in — no claude, no loom-direct-runner)
#     hard-errors non-zero, naming the agent.
#   - Guard passes: WRIX_AGENT=direct with a /workspace/bin shim named
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
# shellcheck source=tests/lib/podman-image.sh
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

WORKSPACE=$(mktemp -d -t wrix-agent-binary-guard.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists "$IMAGE_REF"; then
    podman rmi "$IMAGE_REF" >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF=$(wrix_unique_image_ref "wrix-test-agent-binary-guard")
wrix_load_test_image "$IMAGE_STREAM" "wrix-base-claude" "$IMAGE_REF"

# Run the default entrypoint with no command override ($# -eq 0 inside the
# container) so the agent-exec path — and thus the guard — is reached.
run_agent() {
  local selected_agent="$1"
  local image_agent_override="${2:-}"
  local env_args=(-e "WRIX_AGENT=$selected_agent")
  local volume_args=(-v "$WORKSPACE:/workspace:rw")
  if [[ -n "$image_agent_override" ]]; then
    local image_agent_file="$WORKSPACE/image-agent-$image_agent_override"
    printf '%s\n' "$image_agent_override" >"$image_agent_file"
    volume_args+=(-v "$image_agent_file:/etc/wrix/image-agent:ro")
  fi
  podman run --rm --network=pasta --userns=keep-id \
    "${env_args[@]}" \
    "${volume_args[@]}" \
    "$IMAGE_REF"
}

# --- Case 1: image metadata mismatch -> guard names ProfileConfig/image issue --
rm -rf "${WORKSPACE:?}/bin"
set +e
mismatch_out=$(run_agent direct 2>&1)
mismatch_rc=$?
set -e
[[ "$mismatch_rc" -ne 0 ]] || {
  echo "FAIL: WRIX_AGENT=direct in an image marked claude exited 0 (mismatch guard did not fire)" >&2
  echo "  output: $mismatch_out" >&2
  exit 1
}
case "$mismatch_out" in
  *"ProfileConfig selected WRIX_AGENT=direct"*"built for agent=claude"*"profile_config"*) ;;
  *)
    echo "FAIL: mismatch error did not explain ProfileConfig/image agent mismatch: $mismatch_out" >&2
    exit 1
    ;;
esac

# --- Case 2: selected agent absent -> guard fires, names the agent ------------
set +e
fire_out=$(run_agent claude 2>&1)
fire_rc=$?
set -e
[[ "$fire_rc" -ne 0 ]] || {
  echo "FAIL: WRIX_AGENT=claude against an image without claude exited 0 (guard did not fire)" >&2
  echo "  output: $fire_out" >&2
  exit 1
}
case "$fire_out" in
  *"WRIX_AGENT=claude"*"not present"*) ;;
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

# --- Case 3: selected agent present -> guard passes, agent runs ---------------
mkdir -p "$WORKSPACE/bin"
SENTINEL="WRIX_DIRECT_RAN_OK"
cat > "$WORKSPACE/bin/loom-direct-runner" <<EOF
#!/bin/bash
echo "$SENTINEL"
EOF
chmod +x "$WORKSPACE/bin/loom-direct-runner"

set +e
# Mount matching image-agent metadata here to reuse the lightweight claude test
# image while exercising the binary-presence success path for a direct image.
pass_out=$(run_agent direct direct 2>&1)
pass_rc=$?
set -e
[[ "$pass_rc" -eq 0 ]] || {
  echo "FAIL: WRIX_AGENT=direct with a present binary exited $pass_rc (guard wrongly fired)" >&2
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

echo "PASS: agent-binary-guard (fires on image mismatch and absent binary; passes when present)" >&2
