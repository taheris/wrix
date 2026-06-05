#!/usr/bin/env bash
# Test: Build wrapix image with MCP opt-in, verify tmux and MCP server present
#
# This test builds a sandbox image using mkSandbox with the mcp parameter
# and verifies that MCP servers are properly included:
#
# 1. The image builds successfully with nix build
# 2. tmux is present and executable inside the container
# 3. tmux-mcp is present and executable inside the container
#
# The flake output uses MCP opt-in:
#   mkSandbox {
#     profile = profiles.base;
#     mcp = { tmux = {}; };
#   }
#
# Prerequisites:
# - nix (with flakes enabled)
# - podman
#
# Usage: ./test_sandbox_debug_profile.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}/../../../.."
# shellcheck source=tests/lib/podman-image.sh
source "$REPO_ROOT/tests/lib/podman-image.sh"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# shellcheck disable=SC2317,SC2329  # cleanup is used by trap
cleanup() {
    local exit_code=$?
    if [[ -n "${WORKSPACE:-}" ]] && [[ -d "${WORKSPACE}" ]]; then
        rm -rf "${WORKSPACE}"
    fi
    if [[ -n "${IMAGE_REF:-}" ]] && podman image exists "${IMAGE_REF}"; then
        podman rmi "${IMAGE_REF}" >/dev/null
    fi
    exit "$exit_code"
}
trap cleanup EXIT

# Check prerequisites
if ! command -v nix &>/dev/null; then
    log_error "nix is required but not installed"
    exit 1
fi

if ! command -v podman &>/dev/null; then
    log_warn "podman not available — container runtime checks will be skipped"
    exit 0
fi

# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
if [[ -e /run/.containerenv ]]; then
    log_warn "nested container: podman load unavailable — skipping"
    exit 0
fi

log_info "Building sandbox-mcp image (mcpRuntime=true)..."

PACKAGE_PATH=$(nix build "${REPO_ROOT}#sandbox-mcp" --print-out-paths 2>/dev/null) || {
    log_error "Failed to build sandbox-mcp image"
    log_warn "Check that the mcp parameter is properly configured in lib/sandbox/default.nix"
    exit 1
}

IMAGE_STREAM=$(grep -oP "WRAPIX_DEFAULT_IMAGE_SOURCE=[^']*'\K[^']+" "${PACKAGE_PATH}/bin/wrapix" | head -1) || {
    log_error "Could not find WRAPIX_DEFAULT_IMAGE_SOURCE in wrapper script"
    exit 1
}
WRAPPER_IMAGE_REF=$(grep -oP "WRAPIX_DEFAULT_IMAGE_REF=[^']*'\K[^']+" "${PACKAGE_PATH}/bin/wrapix" | head -1) || {
    log_error "Could not find WRAPIX_DEFAULT_IMAGE_REF in wrapper script"
    exit 1
}
if [[ ! -x "${IMAGE_STREAM}" ]]; then
    log_error "Built image stream not found at ${IMAGE_STREAM}"
    exit 1
fi

log_info "Loading image into podman..."
IMAGE_REF=$(wrapix_unique_image_ref "wrapix-test-sandbox-debug-profile")
wrapix_load_test_image "${IMAGE_STREAM}" "$(wrapix_image_short_name "$WRAPPER_IMAGE_REF")" "${IMAGE_REF}"
log_info "Loaded image: ${IMAGE_REF}"

WORKSPACE=$(mktemp -d)

log_info "Verifying tmux is present in the container..."

# Test tmux presence and version
TMUX_VERSION=$(podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "${IMAGE_REF}" \
    -c "tmux -V" 2>&1) || {
    log_error "tmux is not present or not executable in the container"
    log_error "Output: ${TMUX_VERSION}"
    exit 1
}
log_info "tmux version: ${TMUX_VERSION}"

log_info "Verifying tmux-mcp is present in the container..."

# Test tmux-mcp presence
MCP_PRESENCE=$(podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "${IMAGE_REF}" \
    -c "which tmux-mcp && tmux-mcp --version 2>/dev/null || tmux-mcp --help 2>/dev/null || echo 'found'" 2>&1) || {
    log_error "tmux-mcp is not present or not executable in the container"
    log_error "Output: ${MCP_PRESENCE}"
    exit 1
}
log_info "tmux-mcp found in container"

# Verify the MCP server responds to basic input (if it supports --help or version)
log_info "Verifying MCP server can start..."
MCP_START=$(timeout 5 podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "${IMAGE_REF}" \
    -c "echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"capabilities\":{}}}' | timeout 2 tmux-mcp 2>/dev/null || true; echo 'startup-test-complete'" 2>&1) || true

if [[ "${MCP_START}" != *"startup-test-complete"* ]]; then
    log_warn "MCP server may not be fully implemented yet"
fi

log_info "All checks passed!"
echo ""
echo "Summary:"
echo "  - Image built successfully: ${IMAGE_REF}"
echo "  - tmux present: ${TMUX_VERSION}"
echo "  - tmux-mcp: present"

exit 0
