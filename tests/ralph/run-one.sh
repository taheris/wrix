#!/usr/bin/env bash
# Run a single ralph test by name, with minimal overhead.
# Usage: ./tests/ralph/run-one.sh test_run_closes_issue_on_complete
#        ./tests/ralph/run-one.sh test_render_template_basic --no-dolt
#
# Options:
#   --no-dolt    Skip dolt server setup (for tests that don't need beads DB)
#   --verbose    Show bd/dolt stderr output (not suppressed)
#
# Fast iteration tips:
#   - Keep a shared dolt server running: set SHARED_DOLT_PORT to reuse it
#   - Run without dolt for template/state tests: --no-dolt
#   - Use tab-completion: source <(grep '^test_' tests/ralph/run-tests.sh | sed 's/().*//')
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
export MOCK_CLAUDE="$SCRIPT_DIR/mock-claude"
export SCENARIOS_DIR="$SCRIPT_DIR/scenarios"
LIB_DIR="$SCRIPT_DIR/lib"

source "$LIB_DIR/assertions.sh"
source "$LIB_DIR/fixtures.sh"
source "$LIB_DIR/runner.sh"

init_test_state
setup_colors

# Pre-generate ralph metadata
_ensure_ralph_metadata

# Parse args
TEST_NAME=""
NO_DOLT=false
for arg in "$@"; do
  case "$arg" in
    --no-dolt) NO_DOLT=true ;;
    --verbose) set -x ;;
    test_*) TEST_NAME="$arg" ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if [ -z "$TEST_NAME" ]; then
  echo "Usage: $0 <test_function_name> [--no-dolt] [--verbose]" >&2
  echo "" >&2
  echo "Available tests:" >&2
  grep -oP '^test_\w+(?=\(\))' "$SCRIPT_DIR/run-tests.sh" | sort | head -20 >&2
  echo "  ... ($(grep -cP '^test_\w+\(\)' "$SCRIPT_DIR/run-tests.sh") total)" >&2
  exit 1
fi

# Source run-tests.sh for test function definitions (but don't run main)
# We source it with a guard to prevent the main() call
_SOURCING_FOR_SINGLE_TEST=1
eval "$(sed '/^main "\$@"/d; /^main$/d' "$SCRIPT_DIR/run-tests.sh" | grep -v '^set -euo pipefail$')"

if ! declare -f "$TEST_NAME" >/dev/null 2>&1; then
  echo "ERROR: Test function '$TEST_NAME' not found" >&2
  echo "" >&2
  echo "Did you mean one of these?" >&2
  grep -oP '^test_\w+' "$SCRIPT_DIR/run-tests.sh" | grep -i "${TEST_NAME/test_/}" | head -5 >&2
  exit 1
fi

# Start shared dolt server (unless --no-dolt or already running)
if [ "$NO_DOLT" = false ]; then
  if [ -n "${SHARED_DOLT_PORT:-}" ]; then
    echo "Reusing existing dolt server on port $SHARED_DOLT_PORT"
  else
    setup_shared_dolt_server
    trap teardown_shared_dolt_server EXIT
  fi
  _ensure_beads_snapshot || exit 1
fi

# Run the test directly (no subshell isolation for easier debugging)
echo ""
"$TEST_NAME"
echo ""
echo -e "${GREEN}Test completed.${NC}"
