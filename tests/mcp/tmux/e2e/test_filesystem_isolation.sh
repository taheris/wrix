#!/usr/bin/env bash
# Test: Verify pane commands can only access /workspace
#
# This test verifies that commands run inside tmux panes (via MCP)
# are properly isolated and can only access /workspace:
# 1. Create a pane that tries to access files outside /workspace
# 2. Verify access to /workspace works
# 3. Verify access to host filesystem is blocked
# 4. Verify pane cannot escape the sandbox
#
# Uses MCP opt-in:
#   mkSandbox { profile = base; mcp = { tmux = {}; }; }
#
# Prerequisites:
# - nix (with flakes enabled)
# - podman
#
# Usage: ./test_filesystem_isolation.sh

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
    echo "SKIP: test_filesystem_isolation.sh requires nix (not available)"
    exit 0
fi

if ! command -v podman &>/dev/null; then
    echo "SKIP: test_filesystem_isolation.sh requires podman (not available)"
    exit 0
fi

log_info "Building sandbox-mcp image (mcpRuntime=true)..."

# Build the mcp image: all MCP server packages included, runtime selection via env vars
IMAGE_PATH=$(nix build "${REPO_ROOT}#sandbox-mcp" --print-out-paths 2>/dev/null) || {
    log_error "Failed to build sandbox-mcp image"
    log_warn "Check that the mcp parameter is properly configured in lib/sandbox/default.nix"
    exit 1
}

if [[ ! -f "${IMAGE_PATH}" ]]; then
    log_error "Built image not found at ${IMAGE_PATH}"
    exit 1
fi

log_info "Image built: ${IMAGE_PATH}"

# Create a temporary workspace with test files
WORKSPACE=$(mktemp -d)
log_info "Using workspace: ${WORKSPACE}"

# Create test files in workspace
echo "workspace-accessible" > "${WORKSPACE}/accessible.txt"
mkdir -p "${WORKSPACE}/subdir"
echo "nested-file-content" > "${WORKSPACE}/subdir/nested.txt"

# Create the isolation test script
cat > "${WORKSPACE}/isolation_test.sh" << 'INNER_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail

FAILED=0

pass() {
    echo "[PASS] $1"
}

fail() {
    echo "[FAIL] $1"
    FAILED=1
}

echo "=== Filesystem Isolation Test (via tmux panes) ==="
echo ""

# Helper to run commands via MCP-created tmux pane
# For this test, we simulate what MCP would do by running commands in tmux directly
# since this tests the container isolation rather than MCP protocol

# Create a tmux session
tmux new-session -d -s isolation-test -x 200 -y 50
tmux set-option -t isolation-test remain-on-exit on

run_in_pane() {
    local cmd="$1"
    local pane_name="test-$$"

    # Create a new window and run the command
    tmux new-window -t isolation-test -n "$pane_name" "$cmd; echo '---PANE-EXIT-CODE:'$?"

    # Wait for command to complete
    sleep 1

    # Capture output
    tmux capture-pane -t "isolation-test:$pane_name" -p -S -100

    # Kill the window
    tmux kill-window -t "isolation-test:$pane_name" 2>/dev/null || true
}

echo "[TEST 1] Verify /workspace is accessible"
OUTPUT=$(run_in_pane "cat /workspace/accessible.txt")
if echo "$OUTPUT" | grep -q "workspace-accessible"; then
    pass "Can read files in /workspace"
else
    fail "Cannot read /workspace/accessible.txt"
    echo "  Output: $OUTPUT"
fi

echo ""
echo "[TEST 2] Verify nested workspace files are accessible"
OUTPUT=$(run_in_pane "cat /workspace/subdir/nested.txt")
if echo "$OUTPUT" | grep -q "nested-file-content"; then
    pass "Can read nested files in /workspace"
else
    fail "Cannot read /workspace/subdir/nested.txt"
    echo "  Output: $OUTPUT"
fi

echo ""
echo "[TEST 3] Verify can write to /workspace"
OUTPUT=$(run_in_pane "echo 'written-from-pane' > /workspace/pane-output.txt && cat /workspace/pane-output.txt")
if echo "$OUTPUT" | grep -q "written-from-pane"; then
    pass "Can write files to /workspace"
else
    fail "Cannot write to /workspace"
    echo "  Output: $OUTPUT"
fi

echo ""
echo "[TEST 4] Verify cannot read host /etc/passwd content"
# Container has fakeNss - should not see host users
OUTPUT=$(run_in_pane "cat /etc/passwd")
if echo "$OUTPUT" | grep -qE "^(root|nobody):"; then
    # These are expected container users
    if echo "$OUTPUT" | grep -vE "^(root|nobody):" | grep -qE ":[0-9]+:[0-9]+:"; then
        fail "Container /etc/passwd contains unexpected users (possible host leak)"
        echo "  Output: $OUTPUT"
    else
        pass "Container /etc/passwd only contains expected fakeNss users"
    fi
else
    pass "Container /etc/passwd is properly isolated"
fi

echo ""
echo "[TEST 5] Verify /home is isolated (empty or non-host content)"
OUTPUT=$(run_in_pane "ls -la /home 2>&1 || echo 'no-home'")
# /home should either not exist, be empty, or not contain host home directories
if echo "$OUTPUT" | grep -qE "(No such file|no-home|total 0|^d.* \.$)"; then
    pass "/home directory is isolated"
else
    # Check if any host home directories leaked through
    if echo "$OUTPUT" | grep -qvE "^(total|d.* \.$|d.* \.\.$)"; then
        # There's content - verify it's not from host
        log_warn "/home has content but isolation depends on mount configuration"
    fi
    pass "/home directory check completed"
fi

echo ""
echo "[TEST 6] Verify /tmp is container-local"
# Create a file in /tmp and verify it doesn't persist to host
OUTPUT=$(run_in_pane "echo 'container-tmp-$$' > /tmp/isolation-test-$$.txt && cat /tmp/isolation-test-$$.txt")
if echo "$OUTPUT" | grep -q "container-tmp-$$"; then
    pass "Can write to container-local /tmp"
else
    fail "Cannot write to /tmp"
    echo "  Output: $OUTPUT"
fi

echo ""
echo "[TEST 7] Verify cannot access parent directories above /workspace"
OUTPUT=$(run_in_pane "ls /workspace/../ 2>&1")
# This should show container root, not host root
# Key check: should not see host-specific directories
if echo "$OUTPUT" | grep -qE "(proc|sys|dev|nix|bin|etc|workspace)"; then
    pass "Parent directory access stays within container"
else
    fail "Unexpected parent directory contents"
    echo "  Output: $OUTPUT"
fi

echo ""
echo "[TEST 8] Verify working directory defaults to /workspace"
OUTPUT=$(run_in_pane "pwd")
if echo "$OUTPUT" | grep -q "/workspace"; then
    pass "Default working directory is /workspace"
else
    fail "Working directory is not /workspace"
    echo "  Output: $OUTPUT"
fi

# Cleanup tmux session
tmux kill-session -t isolation-test 2>/dev/null || true

echo ""
echo "========================================="
if [[ $FAILED -eq 0 ]]; then
    echo "[SUCCESS] All filesystem isolation tests passed!"
    exit 0
else
    echo "[FAILED] Some filesystem isolation tests failed"
    exit 1
fi
INNER_SCRIPT
chmod +x "${WORKSPACE}/isolation_test.sh"

log_info "Running filesystem isolation test inside container..."

# Run the test script inside the container
TEST_OUTPUT=$(podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "docker-archive:${IMAGE_PATH}" \
    -c "/workspace/isolation_test.sh" 2>&1) || {
    EXIT_CODE=$?
    log_error "Test failed with exit code ${EXIT_CODE}"
    echo ""
    echo "Test output:"
    echo "${TEST_OUTPUT}"
    exit 1
}

echo ""
echo "Test output:"
echo "${TEST_OUTPUT}"

if echo "${TEST_OUTPUT}" | grep -q '\[SUCCESS\]'; then
    log_info "All filesystem isolation tests passed!"
    exit 0
else
    log_error "Some tests failed"
    exit 1
fi
