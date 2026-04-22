#!/usr/bin/env bash
# Test fixtures for ralph integration tests
# Provides setup_* and teardown_* functions for test environments

# shellcheck disable=SC2034  # Variables may be used externally

#-----------------------------------------------------------------------------
# Ralph Metadata Generation
#-----------------------------------------------------------------------------

# Generate ralph metadata files (variables.json, templates.json) from Nix source.
# Called once per test run when RALPH_METADATA_DIR is not set by the Nix wrapper.
# Caches output in a shared temp dir so nix eval only runs once across all tests.
_ensure_ralph_metadata() {
  # Already generated this run
  if [ -n "${RALPH_METADATA_DIR:-}" ]; then
    return 0
  fi

  local cache_dir="/tmp/ralph-test-metadata-$$"
  if [ -f "$cache_dir/variables.json" ] && [ -f "$cache_dir/templates.json" ]; then
    RALPH_METADATA_DIR="$cache_dir"
    return 0
  fi

  mkdir -p "$cache_dir"
  local template_nix="$REPO_ROOT/lib/ralph/template/default.nix"

  # Generate variables.json from Nix template definitions
  nix eval --impure --raw --expr \
    'let lib = (builtins.getFlake "nixpkgs").lib; tm = import '"$template_nix"' { inherit lib; }; in tm.variablesJson' \
    > "$cache_dir/variables.json" 2>/dev/null || {
    echo "WARNING: Could not generate ralph metadata (nix eval failed)" >&2
    rm -rf "$cache_dir"
    return 1
  }

  # Generate templates.json from Nix template definitions
  nix eval --impure --raw --expr \
    'let lib = (builtins.getFlake "nixpkgs").lib; tm = import '"$template_nix"' { inherit lib; }; in builtins.toJSON (builtins.mapAttrs (n: t: t.variables) tm.templates)' \
    > "$cache_dir/templates.json" 2>/dev/null || {
    echo "WARNING: Could not generate ralph metadata (nix eval failed)" >&2
    rm -rf "$cache_dir"
    return 1
  }

  RALPH_METADATA_DIR="$cache_dir"
}

#-----------------------------------------------------------------------------
# Shared Dolt Server (sourced from tests/lib/)
#-----------------------------------------------------------------------------

# Resolve shared lib relative to this file's location.
# When run from the Nix store (via ralphTestDir), the lib is bundled at lib-shared/.
# When run directly from the repo, it's at ../../lib/.
_FIXTURES_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$_FIXTURES_DIR/../lib-shared/dolt-server.sh" ]; then
  source "$_FIXTURES_DIR/../lib-shared/dolt-server.sh"
elif [ -f "$_FIXTURES_DIR/../../lib/dolt-server.sh" ]; then
  source "$_FIXTURES_DIR/../../lib/dolt-server.sh"
else
  echo "ERROR: Cannot find tests/lib/dolt-server.sh" >&2
  exit 1
fi

#-----------------------------------------------------------------------------
# Test Environment Setup
#-----------------------------------------------------------------------------

# Create isolated test environment
# Usage: setup_test_env <test_name>
# Sets: TEST_DIR, BD_DB, RALPH_DIR, RALPH_TEMPLATE_DIR, PATH
setup_test_env() {
  local test_name="$1"

  # Save original directory and PATH FIRST (before any modifications)
  ORIGINAL_DIR="$PWD"
  ORIGINAL_PATH="$PATH"

  # Create temp directory
  TEST_DIR=$(mktemp -d -t "ralph-test-$test_name-XXXXXX")
  export TEST_DIR

  # Create project structure
  mkdir -p "$TEST_DIR/specs"
  mkdir -p "$TEST_DIR/docs"
  mkdir -p "$TEST_DIR/.wrapix/ralph/state"
  mkdir -p "$TEST_DIR/.wrapix/ralph/logs"
  mkdir -p "$TEST_DIR/.wrapix/ralph/template"
  mkdir -p "$TEST_DIR/.beads"
  chmod 700 "$TEST_DIR/.beads"

  # Create minimal docs/README.md (session-start pin)
  cat > "$TEST_DIR/docs/README.md" << 'EOF'
# Project Overview

## Specs

| Spec | Bead | Purpose |
|------|------|---------|
EOF

  # Create minimal ralph config
  cat > "$TEST_DIR/.wrapix/ralph/config.nix" << 'EOF'
{
  pinnedContext = "docs/README.md";
  beads.priority = 2;
}
EOF

  # Create run.md template
  cat > "$TEST_DIR/.wrapix/ralph/template/run.md" << 'EOF'
# Implementation Step

## Context Pinning

First, read the project overview to understand project terminology and context:

{{PINNED_CONTEXT}}

## Current Spec

Read: {{SPEC_PATH}}

## Issue Details

Issue: {{ISSUE_ID}}
Title: {{TITLE}}
Molecule: {{MOLECULE_ID}}

{{DESCRIPTION}}

## Instructions

1. **Understand**: Read the spec and issue thoroughly before making changes
2. **Implement**: Write code following the spec
3. **Discovered Work**: If you find tasks outside this issue's scope:
   - Create with: `bd create --title="..." --labels="spec-{{LABEL}}"`
   - Bond to molecule: `bd mol bond {{MOLECULE_ID}} <new-issue>`
4. **Quality Gates**: Before completing, ensure:
   - [ ] All tests pass
   - [ ] Lint checks pass

## Exit Signals

Output ONE of these when done:

- `RALPH_COMPLETE` - Task finished, all quality gates passed
- `RALPH_BLOCKED: <reason>` - Cannot proceed, explain why
- `RALPH_CLARIFY: <question>` - Need clarification before proceeding
EOF

  # Create todo.md template
  cat > "$TEST_DIR/.wrapix/ralph/template/todo.md" << 'EOF'
# Convert Spec to Tasks

Read: {{SPEC_PATH}}

Label: spec-{{LABEL}}
Priority: {{PRIORITY}}
Spec Title: {{SPEC_TITLE}}

## Instructions

1. Read the spec thoroughly
2. Create an epic bead for the overall feature
3. Create task beads for each implementation task
4. Add dependencies between tasks

{{README_INSTRUCTIONS}}

{{README_UPDATE_SECTION}}

## Exit Signals

Output `RALPH_COMPLETE` when all issues are created.
EOF

  # Create plan.md template
  cat > "$TEST_DIR/.wrapix/ralph/template/plan.md" << 'EOF'
# Specification Interview

You are conducting a specification interview.

## Context (from docs/README.md)

{{PINNED_CONTEXT}}

## Current Feature

Label: {{LABEL}}
Spec file: {{SPEC_PATH}}

## Interview Guidelines

1. Ask questions to understand the feature
2. When you have enough information, create the spec

## Output Actions

When you have gathered enough information, create:

1. **Spec file** at `{{SPEC_PATH}}`

{{README_INSTRUCTIONS}}

{{README_UPDATE_SECTION}}

## Exit Signals

Output ONE of these at the end of your response:

- `RALPH_COMPLETE` - Interview finished, spec created
- `RALPH_BLOCKED: <reason>` - Cannot proceed
- `RALPH_CLARIFY: <question>` - Need clarification
EOF

  # Create bin directory with mock claude as 'claude'
  mkdir -p "$TEST_DIR/bin"
  ln -sf "$MOCK_CLAUDE" "$TEST_DIR/bin/claude"

  # Symlink ralph commands from SOURCE (not installed) to test latest code
  # This ensures tests verify the actual source, not a potentially stale build
  RALPH_SRC_DIR="$REPO_ROOT/lib/ralph/cmd"
  for cmd in ralph-run ralph-todo ralph-plan ralph-status ralph-sync ralph-check ralph-spec ralph-use ralph-logs ralph-msg; do
    local script_name="${cmd#ralph-}"  # Remove 'ralph-' prefix
    if [ -f "$RALPH_SRC_DIR/$script_name.sh" ]; then
      ln -sf "$RALPH_SRC_DIR/$script_name.sh" "$TEST_DIR/bin/$cmd"
    fi
  done

  # Symlink util.sh from source
  if [ -f "$RALPH_SRC_DIR/util.sh" ]; then
    ln -sf "$RALPH_SRC_DIR/util.sh" "$TEST_DIR/bin/util.sh"
  fi

  # Symlink other required commands from installed location
  # Include core utilities that may be in the wrapix profile
  for cmd in bd jq nix grep cat sed awk mkdir rm cp mv ls chmod touch date script echo diff git; do
    if cmd_path=$(command -v "$cmd" 2>/dev/null); then
      ln -sf "$cmd_path" "$TEST_DIR/bin/$cmd"
    fi
  done

  # Filter PATH to remove wrapix-related paths (prevents using installed claude/ralph)
  # Two cases:
  # 1. Outside container: wrapix binary exists - filter to prevent container re-exec
  # 2. Inside container: wrapix-profile-env has real claude/ralph - filter to use mocks
  FILTERED_PATH=""
  IFS=':' read -ra PATH_PARTS <<< "$PATH"
  for part in "${PATH_PARTS[@]}"; do
    # Skip paths containing wrapix binary (prevents container re-launch)
    if [ -x "$part/wrapix" ]; then
      continue
    fi
    # Skip wrapix-profile-env paths (these have real claude/ralph, not mocks)
    if [[ "$part" == *"wrapix-profile-env"* ]]; then
      continue
    fi
    if [ -n "$FILTERED_PATH" ]; then
      FILTERED_PATH="$FILTERED_PATH:$part"
    else
      FILTERED_PATH="$part"
    fi
  done

  # Set up PATH with test bin first, excluding wrapix locations
  export PATH="$TEST_DIR/bin:$FILTERED_PATH"

  # Change to test dir
  cd "$TEST_DIR" || return 1

  # Set ralph directory
  export RALPH_DIR=".wrapix/ralph"

  # Set template directory for diff/sync/check commands
  export RALPH_TEMPLATE_DIR="$REPO_ROOT/lib/ralph/template"

  # Set RALPH_METADATA_DIR for template functions (get_template_variables, get_variable_definitions).
  # When run via `nix run .#test`, the Nix wrapper pre-sets this. When run directly
  # (bash run-tests.sh), generate the metadata from Nix source once and cache it.
  if [ -z "${RALPH_METADATA_DIR:-}" ]; then
    _ensure_ralph_metadata
  fi
  export RALPH_METADATA_DIR

  echo "  Test environment: $TEST_DIR"
}

# Initialize beads database for tests that need it.
# Must be called AFTER setup_test_env. Creates a unique database on the shared
# dolt server so tests get full isolation without affecting pure tests.
# Usage: init_beads
init_beads() {
  export BD_DB="$TEST_DIR/.beads/issues.db"
  mkdir -p "$(dirname "$BD_DB")"

  # Suppress dolt sql-server auto-start in test subprocesses.
  # Without this, any bd command that briefly fails to connect to the shared
  # server (race condition, slow start) will spawn its own detached dolt
  # sql-server — creating orphaned processes that are never cleaned up.
  export BEADS_DOLT_AUTO_START=0
  if [ -n "${SHARED_DOLT_PORT:-}" ]; then
    export BEADS_DOLT_SERVER_PORT="$SHARED_DOLT_PORT"
  fi

  local test_prefix
  test_prefix="t$(echo "${CURRENT_TEST:-unknown}" | tr -cd 'a-z0-9' | head -c 6)${RANDOM}"
  if [ -n "${SHARED_DOLT_PORT:-}" ]; then
    (cd "$TEST_DIR" && bd init --prefix "$test_prefix" \
      --server-port "$SHARED_DOLT_PORT" --skip-hooks --quiet >/dev/null 2>&1) || {
      echo "WARNING: bd init failed for ${CURRENT_TEST:-unknown}" >&2
    }
  fi
}

# Clean up test environment
# Usage: teardown_test_env
teardown_test_env() {
  # Return to original directory and PATH
  cd "$ORIGINAL_DIR" 2>/dev/null || true
  export PATH="$ORIGINAL_PATH"

  # Clean up temp directory
  if [ -n "${TEST_DIR:-}" ] && [ -d "$TEST_DIR" ]; then
    rm -rf "$TEST_DIR"
  fi

  # Unset test environment variables
  unset TEST_DIR BD_DB MOCK_SCENARIO RALPH_DIR RALPH_TEMPLATE_DIR BEADS_DOLT_AUTO_START BEADS_DOLT_SERVER_PORT
}

#-----------------------------------------------------------------------------
# Spec File Fixtures
#-----------------------------------------------------------------------------

# Create a minimal spec file for testing
# Usage: create_test_spec <label> [content]
create_test_spec() {
  local label="$1"
  local content="${2:-}"

  if [ -z "$content" ]; then
    content="# Test Feature

## Requirements
- Implement the test feature
"
  fi

  echo "$content" > "$TEST_DIR/specs/$label.md"
}

# Set up per-label state file and active workflow pointer
# Writes state/<label>.json and state/current (plain text)
# Usage: setup_label_state <label> [hidden] [molecule]
setup_label_state() {
  local label="$1"
  local hidden="${2:-false}"
  local molecule="${3:-}"

  # Compute spec_path based on hidden flag
  local spec_path
  if [ "$hidden" = "true" ]; then
    spec_path="$RALPH_DIR/state/$label.md"
  else
    spec_path="specs/$label.md"
  fi

  local json="{\"label\":\"$label\",\"update\":false,\"hidden\":$hidden,\"spec_path\":\"$spec_path\""
  if [ -n "$molecule" ]; then
    json="$json,\"molecule\":\"$molecule\""
  fi
  json="$json}"

  mkdir -p "$RALPH_DIR/state"
  echo "$json" > "$RALPH_DIR/state/${label}.json"
  echo "$label" > "$RALPH_DIR/state/current"
}

#-----------------------------------------------------------------------------
# Mock BD Setup
#-----------------------------------------------------------------------------

# Create a mock bd command that logs invocations and returns controlled output
# Usage: setup_mock_bd <log_file> [mock_responses_dir]
setup_mock_bd() {
  local log_file="$1"
  local mock_responses_dir="${2:-}"
  local bin_dir="${TEST_DIR:-/tmp}/bin"

  # Save the real bd path before overwriting
  if [ -L "$bin_dir/bd" ]; then
    export REAL_BD_PATH
    REAL_BD_PATH=$(readlink -f "$bin_dir/bd" 2>/dev/null || true)
    # Remove the symlink so we can create our mock
    rm -f "$bin_dir/bd"
  fi

  # Create mock bd script
  cat > "$bin_dir/bd" << 'MOCK_BD_EOF'
#!/usr/bin/env bash
# Mock bd command for testing
set -euo pipefail

LOG_FILE="${BD_MOCK_LOG:-/tmp/bd-mock.log}"
MOCK_RESPONSES="${BD_MOCK_RESPONSES:-}"

# Log the invocation
echo "bd $*" >> "$LOG_FILE"

# Handle mol subcommands
if [ "${1:-}" = "mol" ]; then
  subcommand="${2:-}"
  molecule="${3:-}"

  case "$subcommand" in
    progress)
      # Check for --json flag
      if [[ " $* " == *" --json "* ]]; then
        # Check for custom JSON response
        if [ -n "$MOCK_RESPONSES" ] && [ -f "$MOCK_RESPONSES/mol-progress.json" ]; then
          cat "$MOCK_RESPONSES/mol-progress.json"
          exit 0
        fi
        # Default JSON mock response
        cat << EOF
{
  "completed": 8,
  "current_step_id": "test-step-3",
  "in_progress": 1,
  "molecule_id": "$molecule",
  "molecule_title": "Test Molecule",
  "percent": 80,
  "total": 10
}
EOF
        exit 0
      fi
      # Check for custom text response
      if [ -n "$MOCK_RESPONSES" ] && [ -f "$MOCK_RESPONSES/mol-progress.txt" ]; then
        cat "$MOCK_RESPONSES/mol-progress.txt"
        exit 0
      fi
      # Default text mock response
      cat << EOF
Molecule: $molecule (Test Molecule)
Progress: 8 / 10 (80%)
Current step: test-step-3
EOF
      exit 0
      ;;
    current)
      # Check for custom response
      if [ -n "$MOCK_RESPONSES" ] && [ -f "$MOCK_RESPONSES/mol-current.txt" ]; then
        cat "$MOCK_RESPONSES/mol-current.txt"
        exit 0
      fi
      # Default mock response
      cat << EOF
[done]    Setup project structure
[done]    Implement core feature
[current] Write tests         <- you are here
[ready]   Update documentation
[blocked] Final review (waiting on tests)
EOF
      exit 0
      ;;
    stale)
      # Check for custom response
      if [ -n "$MOCK_RESPONSES" ] && [ -f "$MOCK_RESPONSES/mol-stale.txt" ]; then
        cat "$MOCK_RESPONSES/mol-stale.txt"
        exit 0
      fi
      # Check for --quiet flag
      if [[ " $* " == *" --quiet "* ]]; then
        # Default: no stale molecules (empty output)
        exit 0
      fi
      echo "No stale molecules found"
      exit 0
      ;;
    *)
      echo "Mock bd: unknown mol subcommand: $subcommand" >&2
      exit 1
      ;;
  esac
fi

# For non-mol commands, pass through to real bd if available
REAL_BD="${REAL_BD_PATH:-}"
if [ -n "$REAL_BD" ] && [ -x "$REAL_BD" ]; then
  exec "$REAL_BD" "$@"
fi

echo "Mock bd: no real bd available for: $*" >&2
exit 1
MOCK_BD_EOF

  chmod +x "$bin_dir/bd"

  # Export environment variables for the mock
  export BD_MOCK_LOG="$log_file"
  export BD_MOCK_RESPONSES="$mock_responses_dir"
}

# Verify mock bd was called with expected arguments
# Usage: assert_bd_called <expected_pattern> <log_file> [message]
assert_bd_called() {
  local expected="$1"
  local log_file="$2"
  local msg="${3:-bd should be called with: $expected}"

  if grep -q "$expected" "$log_file" 2>/dev/null; then
    test_pass "$msg"
    return 0
  else
    test_fail "$msg"
    echo "  Expected call matching: $expected"
    echo "  Log contents:"
    cat "$log_file" 2>/dev/null | sed 's/^/    /' || echo "    (empty)"
    return 1
  fi
}
