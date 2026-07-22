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

cleanup() {
  local ref
  for ref in "${PODMAN_IMAGE_REFS[@]}"; do
    if command -v podman >/dev/null 2>&1 && podman image exists "$ref"; then
      podman rmi "$ref" >/dev/null 2>&1 || true # best-effort: cleanup must not mask the verifier result when an image is pinned.
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

test_darwin_container_starts() {
  wrix_require_live_sandbox_darwin
  cd "$REPO_ROOT"

  local command_line result sandbox workspace
  local -a command
  sandbox=$(wrix_build_packaged_live_sandbox)
  workspace="$TEST_TMP/darwin-workspace"
  mkdir -p "$workspace"
  command=(
    "$sandbox/bin/wrix" run "$workspace"
    /bin/bash -c 'printf "container-started\\n"; grep localhost /etc/hosts'
  )
  printf -v command_line '%q ' "${command[@]}"

  result=$(wrix_run_with_pty "$command_line")
  assert_contains "darwin start" "$result" "container-started" || return 1
  assert_contains "darwin loopback" "$result" "localhost" || return 1

  printf 'PASS: packaged Darwin sandbox container-start\n' >&2
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
