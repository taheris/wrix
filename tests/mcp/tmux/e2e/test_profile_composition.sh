#!/usr/bin/env bash
# Test: Build rust profile with MCP opt-in, verify both rust toolchain and debug tools available
#
# This test verifies MCP opt-in composition with language profiles:
# 1. Build rust + tmux using MCP opt-in:
#    mkSandbox { profile = rust; mcp = { tmux = {}; }; }
# 2. Verify rust toolchain is present (rustc, cargo)
# 3. Verify debug tools are present (tmux, tmux-mcp)
# 4. Verify all base tools are still present
#
# This demonstrates that MCP servers can be added to any profile
# without requiring dedicated debug variants.
#
# Prerequisites:
# - nix (with flakes enabled)
# - podman
#
# Usage: ./test_profile_composition.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}/../../.."

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
    exit "$exit_code"
}
trap cleanup EXIT

# Check prerequisites — skip gracefully if not available
if ! command -v nix &>/dev/null; then
    echo "SKIP: test_profile_composition.sh requires nix (not available)"
    exit 0
fi

HAS_PODMAN=true
if ! command -v podman &>/dev/null; then
    HAS_PODMAN=false
    log_warn "podman not available — container runtime checks will be skipped"
fi

log_info "Building sandbox-rust-mcp image (mcpRuntime=true)..."

# Build the rust + mcp image: all MCP server packages included, runtime selection via env vars
PACKAGE_PATH=$(nix build "${REPO_ROOT}#sandbox-rust-mcp" --print-out-paths 2>/dev/null) || {
    log_error "Failed to build sandbox-rust-mcp image"
    log_warn "MCP opt-in composition may not be configured correctly"
    log_warn "Check that the mcp parameter is properly handled in lib/sandbox/default.nix"
    exit 1
}

if [[ ! -d "${PACKAGE_PATH}" ]]; then
    log_error "Built package not found at ${PACKAGE_PATH}"
    exit 1
fi

# Extract the image stream path from the wrapper script
# The launcher pipes the stream script to podman: /nix/store/xxx | podman load -q
IMAGE_PATH=$(grep -oP '/nix/store/\S+(?= \| podman load)' "${PACKAGE_PATH}/bin/wrapix" | head -1) || {
    log_error "Could not find image stream path in wrapper script"
    exit 1
}

if [[ ! -e "${IMAGE_PATH}" ]]; then
    log_error "Image stream not found at ${IMAGE_PATH}"
    exit 1
fi

log_info "Image built successfully: ${IMAGE_PATH}"
log_info "PASS: Profile composition (rust + tmux MCP) builds correctly"

if [[ "$HAS_PODMAN" != "true" ]]; then
    log_info "SKIP: Container runtime checks skipped (podman not available)"
    log_info "Composition validated via successful nix build"
    exit 0
fi

# Create a temporary workspace
WORKSPACE=$(mktemp -d)
log_info "Using workspace: ${WORKSPACE}"

FAILED=0

# Helper function to check if a command exists in the container
check_command() {
    local cmd="$1"
    local description="${2:-$1}"

    if podman run --rm \
        --network=pasta \
        --userns=keep-id \
        --entrypoint /bin/bash \
        -v "${WORKSPACE}:/workspace:rw" \
        -w /workspace \
        "docker-archive:${IMAGE_PATH}" \
        -c "which $cmd" &>/dev/null; then
        log_info "PASS: $description is present"
        return 0
    else
        log_error "FAIL: $description is NOT present"
        FAILED=1
        return 1
    fi
}

# Helper function to run a command and check output
check_command_output() {
    local cmd="$1"
    local expected="$2"
    local description="$3"

    local output
    output=$(podman run --rm \
        --network=pasta \
        --userns=keep-id \
        --entrypoint /bin/bash \
        -v "${WORKSPACE}:/workspace:rw" \
        -w /workspace \
        "docker-archive:${IMAGE_PATH}" \
        -c "$cmd" 2>&1) || true

    if echo "$output" | grep -qi "$expected"; then
        log_info "PASS: $description"
        return 0
    else
        log_error "FAIL: $description"
        log_error "  Expected to find: $expected"
        log_error "  Got: $output"
        FAILED=1
        return 1
    fi
}

echo ""
log_info "=== Checking Rust toolchain ==="

# Check rustc (Rust compiler) - provided by fenix
check_command "rustc" "rustc (Rust compiler)"
check_command_output "rustc --version" "rustc" "rustc is functional"

# Check cargo (Rust package manager)
check_command "cargo" "cargo (Rust package manager)"

# Verify CARGO_HOME and RUST_SRC_PATH environment variables are set
check_command_output "echo \$CARGO_HOME" "/home/wrapix/.cargo" "CARGO_HOME is set correctly"
check_command_output "echo \$RUST_SRC_PATH" "rustlib/src/rust/library" "RUST_SRC_PATH is set correctly"

echo ""
log_info "=== Checking debug tools ==="

# Check tmux
check_command "tmux" "tmux terminal multiplexer"
check_command_output "tmux -V" "tmux" "tmux is executable"

# Check tmux-mcp
check_command "tmux-mcp" "tmux-mcp MCP server"

echo ""
log_info "=== Checking base profile tools ==="

# Essential base tools
check_command "git" "git"
check_command "bash" "bash"
check_command "jq" "jq"
check_command "curl" "curl"
check_command "ripgrep" "ripgrep (rg)" || check_command "rg" "ripgrep (rg)"
check_command "fd" "fd"

echo ""
log_info "=== Checking Rust development dependencies ==="

# Check OpenSSL is available (commonly needed for Rust builds)
check_command_output "echo \$OPENSSL_LIB_DIR" "openssl" "OPENSSL_LIB_DIR is set"

# Check pkg-config (needed for many native dependencies)
check_command "pkg-config" "pkg-config"

# Check gcc (needed for linking)
check_command "gcc" "gcc"

echo ""
log_info "=== MCP opt-in composition validation ==="

# Verify that rust profile env vars and MCP server are both present
log_info "Checking that rust profile environment and MCP packages are merged..."

# Create a test script to dump all relevant env vars
cat > "${WORKSPACE}/check_env.sh" << 'SCRIPT'
#!/usr/bin/env bash
echo "=== Environment Variables ==="
echo "CARGO_HOME=$CARGO_HOME"
echo "RUST_SRC_PATH=$RUST_SRC_PATH"
echo "OPENSSL_LIB_DIR=$OPENSSL_LIB_DIR"
echo "OPENSSL_INCLUDE_DIR=$OPENSSL_INCLUDE_DIR"
echo "PATH=$PATH"
echo ""
echo "=== Tools availability ==="
echo "rustc: $(which rustc 2>/dev/null || echo 'not found')"
echo "tmux: $(which tmux 2>/dev/null || echo 'not found')"
echo "tmux-mcp: $(which tmux-mcp 2>/dev/null || echo 'not found')"
echo "git: $(which git 2>/dev/null || echo 'not found')"
SCRIPT
chmod +x "${WORKSPACE}/check_env.sh"

ENV_OUTPUT=$(podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "docker-archive:${IMAGE_PATH}" \
    -c "/workspace/check_env.sh" 2>&1)

echo ""
echo "Environment dump:"
echo "$ENV_OUTPUT"

echo ""
echo "========================================="
if [[ $FAILED -eq 0 ]]; then
    log_info "SUCCESS: All MCP opt-in composition checks passed!"
    echo ""
    echo "Summary:"
    echo "  - rust + tmux MCP opt-in builds successfully"
    echo "  - Rust toolchain (rustc, cargo) available"
    echo "  - MCP server (tmux, tmux-mcp) available"
    echo "  - Base profile tools (git, jq, curl, etc.) available"
    echo "  - Rust environment and MCP packages merged correctly"
    exit 0
else
    log_error "FAILED: Some MCP opt-in composition checks failed"
    exit 1
fi
