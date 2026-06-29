#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
# shellcheck source=tests/lib/podman-image.sh
source "$SCRIPT_DIR/../lib/podman-image.sh"

TEST_TMP="$(mktemp -d -t wrix-container-start.XXXXXX)"
PODMAN_IMAGE_REFS=()
CONTAINER_IMAGE_REFS=()

cleanup() {
  local ref
  for ref in "${PODMAN_IMAGE_REFS[@]}"; do
    if command -v podman >/dev/null 2>&1 && podman image exists "$ref"; then
      podman rmi "$ref" >/dev/null 2>&1 || true # best-effort: cleanup must not mask the verifier result when an image is pinned.
    fi
  done
  for ref in "${CONTAINER_IMAGE_REFS[@]}"; do
    if command -v container >/dev/null 2>&1 && container image inspect "$ref" >/dev/null 2>&1; then
      container image delete "$ref" >/dev/null 2>&1 || true # best-effort: cleanup must not mask the verifier result when an image is pinned.
    fi
  done
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

test_linux_container_starts() {
  wrix_require_live_sandbox_linux
  cd "$REPO_ROOT"

  local image_source workspace image_ref result
  image_source=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-base)
  workspace="$TEST_TMP/linux-workspace"
  mkdir -p "$workspace"
  image_ref=$(wrix_unique_image_ref "wrix-test-container-starts")
  PODMAN_IMAGE_REFS+=("$image_ref")
  wrix_load_test_image "$image_source" "wrix-base-claude" "$image_ref"

  result=$(podman run --rm --network=pasta --userns=keep-id \
    --entrypoint /bin/bash \
    -v "$workspace:/workspace:rw" \
    -w /workspace \
    "$image_ref" \
    -c "echo container-started")
  assert_contains "linux start" "$result" "container-started" || return 1

  result=$(podman run --rm --network=pasta --userns=keep-id \
    --entrypoint /bin/bash \
    -v "$workspace:/workspace:rw" \
    "$image_ref" \
    -c "grep localhost /etc/hosts")
  assert_contains "linux loopback" "$result" "localhost" || return 1

  printf 'PASS: linux container-start\n' >&2
}

loaded_ref_from_container_load() {
  local load_output="$1"
  local loaded_ref
  loaded_ref=$(printf '%s\n' "$load_output" | sed -n 's/.*\(untagged@sha256:[a-f0-9]\{64\}\).*/\1/p' | head -n 1)
  if [[ -n "$loaded_ref" ]]; then
    printf '%s\n' "$loaded_ref"
    return 0
  fi
  loaded_ref=$(printf '%s\n' "$load_output" | sed -n 's/^Loaded image: //p; s/^Loaded image(s): //p' | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | head -n 1)
  [[ -n "$loaded_ref" ]] || return 1
  printf '%s\n' "$loaded_ref"
}

test_darwin_container_starts() {
  wrix_require_live_sandbox_darwin
  command -v skopeo >/dev/null 2>&1 || wrix_live_skip "skopeo not on PATH"
  cd "$REPO_ROOT"

  local image_source workspace image_ref oci_tar load_output loaded_ref result
  image_source=$(wrix_realize_test_image_source direct)
  workspace="$TEST_TMP/darwin-workspace"
  mkdir -p "$workspace"
  image_ref=$(wrix_live_image_ref "container-starts-$$")
  CONTAINER_IMAGE_REFS+=("$image_ref")
  oci_tar="$TEST_TMP/darwin-container-starts.oci.tar"

  skopeo --insecure-policy copy --quiet "docker-archive:$image_source" "oci-archive:$oci_tar"
  load_output=$(container image load --input "$oci_tar" 2>&1)
  if ! loaded_ref=$(loaded_ref_from_container_load "$load_output"); then
    fail "could not determine loaded Darwin image ref: $load_output"
    return 1
  fi
  container image tag "$loaded_ref" "$image_ref"

  result=$(container run --rm \
    -w / \
    -v "$workspace:/workspace" \
    -- \
    "$image_ref" \
    /bin/bash -c "echo container-started")
  assert_contains "darwin start" "$result" "container-started" || return 1

  result=$(container run --rm \
    -w / \
    -v "$workspace:/workspace" \
    -- \
    "$image_ref" \
    /bin/bash -c "grep localhost /etc/hosts")
  assert_contains "darwin loopback" "$result" "localhost" || return 1

  printf 'PASS: darwin container-start\n' >&2
}

test_current_platform_container_starts() {
  local uname_s
  uname_s=$(uname -s)
  case "$uname_s" in
    Linux) test_linux_container_starts ;;
    Darwin) test_darwin_container_starts ;;
    *) wrix_live_skip "unsupported live sandbox host: $uname_s" ;;
  esac
}

if [[ "$#" -eq 0 ]]; then
  test_current_platform_container_starts
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
