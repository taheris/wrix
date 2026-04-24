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

  # Create entire project structure in one mkdir invocation
  mkdir -p \
    "$TEST_DIR/specs" \
    "$TEST_DIR/docs" \
    "$TEST_DIR/bin" \
    "$TEST_DIR/.wrapix/ralph/state" \
    "$TEST_DIR/.wrapix/ralph/logs" \
    "$TEST_DIR/.wrapix/ralph/template" \
    "$TEST_DIR/.beads"
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
   - Create with: `bd create --title="..." --labels="spec:{{LABEL}}"`
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

Label: spec:{{LABEL}}
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

  RALPH_SRC_DIR="$REPO_ROOT/lib/ralph/cmd"
  local script_name
  for script_name in run todo plan status sync check spec use logs msg init; do
    ln -sf "$RALPH_SRC_DIR/$script_name.sh" "$TEST_DIR/bin/ralph-$script_name"
  done

  local -a same_name_srcs=("$RALPH_SRC_DIR/util.sh")
  local util_cmd util_path
  for util_cmd in bd jq nix grep cat sed awk mkdir rm cp mv ls chmod touch date script echo diff git; do
    if util_path=$(command -v "$util_cmd" 2>/dev/null); then
      same_name_srcs+=("$util_path")
    fi
  done
  ln -sf "${same_name_srcs[@]}" "$TEST_DIR/bin/"
  ln -sf "$MOCK_CLAUDE" "$TEST_DIR/bin/claude"

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

# Run 'bd init' once per test-run and expose the result as RALPH_BEADS_SNAPSHOT;
# init_beads cp -a's this template instead of paying bd init's cost per test.
# Snapshot lives under $SHARED_DOLT_DIR so teardown_shared_dolt_server cleans it up.
_ensure_beads_snapshot() {
  if [ -n "${RALPH_BEADS_SNAPSHOT:-}" ] && [ -d "$RALPH_BEADS_SNAPSHOT" ]; then
    return 0
  fi

  if [ -z "${SHARED_DOLT_DIR:-}" ] || [ ! -d "$SHARED_DOLT_DIR" ]; then
    echo "ERROR: _ensure_beads_snapshot requires SHARED_DOLT_DIR; call setup_shared_dolt_server first" >&2
    return 1
  fi

  local snap_root="$SHARED_DOLT_DIR/beads-snapshot"
  if [ ! -d "$snap_root/.beads" ]; then
    mkdir -p "$snap_root"
    if ! (cd "$snap_root" && bd init --prefix ralphsnap \
            ${SHARED_DOLT_PORT:+--server-port "$SHARED_DOLT_PORT"} \
            --skip-hooks --skip-agents --non-interactive --quiet </dev/null); then
      echo "ERROR: bd init failed while creating beads snapshot at $snap_root" >&2
      rm -rf "$snap_root"
      return 1
    fi
  fi

  RALPH_BEADS_SNAPSHOT="$snap_root/.beads"
  export RALPH_BEADS_SNAPSHOT
}

# Initialize beads database for tests that need it.
# Must be called AFTER setup_test_env. Populates TEST_DIR/.beads from the shared
# snapshot (see _ensure_beads_snapshot); each TEST_DIR still holds its own
# embedded Dolt DB, so tests remain isolated without re-running bd init.
# Usage: init_beads
init_beads() {
  export BD_DB="$TEST_DIR/.beads/issues.db"

  # Suppress dolt sql-server auto-start in test subprocesses.
  # Without this, any bd command that briefly fails to connect to the shared
  # server (race condition, slow start) will spawn its own detached dolt
  # sql-server — creating orphaned processes that are never cleaned up.
  export BEADS_DOLT_AUTO_START=0
  if [ -n "${SHARED_DOLT_PORT:-}" ]; then
    export BEADS_DOLT_SERVER_PORT="$SHARED_DOLT_PORT"
  fi

  if [ -z "${SHARED_DOLT_PORT:-}" ]; then
    mkdir -p "$(dirname "$BD_DB")"
    return 0
  fi

  if ! _ensure_beads_snapshot; then
    echo "WARNING: beads snapshot unavailable for ${CURRENT_TEST:-unknown}" >&2
    return 1
  fi

  rm -rf "$TEST_DIR/.beads"
  cp -a "$RALPH_BEADS_SNAPSHOT" "$TEST_DIR/.beads"
}

# Assign monotonically increasing created_at timestamps to bead IDs in argument
# order. bd's storage layer formats timestamps as time.RFC3339 (second precision;
# see beads internal/storage/dolt/transaction.go), so rapid bd create calls can
# produce identical created_at values and a non-deterministic sort. This helper
# writes created_at directly via dolt sql against the embedded Dolt DB so
# ordering-sensitive tests don't need sleeps between creates. `bd sql` refuses
# to run in embedded mode, so we bypass bd and talk to Dolt directly.
#
# Usage: set_monotonic_created_at <id1> [<id2> ...]
set_monotonic_created_at() {
  local dolt_dir="$TEST_DIR/.beads/embeddeddolt/ralphsnap"
  if [ ! -d "$dolt_dir/.dolt" ]; then
    echo "ERROR: set_monotonic_created_at: embedded dolt dir missing at $dolt_dir" >&2
    return 1
  fi
  local i=0
  local id
  for id in "$@"; do
    (cd "$dolt_dir" && dolt sql -q \
      "UPDATE issues SET created_at = '2000-01-01 00:00:0${i}' WHERE id = '${id}'" \
      >/dev/null)
    i=$((i + 1))
  done
}

# Simulate Claude dispatching the SessionStart[compact] re-pin hook and emit
# hookSpecificOutput.additionalContext on stdout.
#
# Locates claude-settings.json in this order:
#   1. $RALPH_DIR/runtime/<label>/claude-settings.json (if label given)
#   2. $RALPH_RUNTIME_DIR/claude-settings.json (set by install_repin_hook)
#   3. The unique $RALPH_DIR/runtime/*/claude-settings.json under the test tree
#
# Extracts the SessionStart entry with matcher="compact", resolves its command
# (rewriting the container /workspace/... path to the test-tree runtime dir
# when needed), executes it, and returns hookSpecificOutput.additionalContext.
#
# Exits nonzero with a message to stderr if:
#   - settings file missing or not readable
#   - no SessionStart[compact] hook registered
#   - the hook script is not executable or exits non-zero
#   - the hook output is not valid JSON or lacks additionalContext
#
# Usage: simulate_compact_event [label]
simulate_compact_event() {
  local label="${1:-}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local settings=""
  local runtime_dir=""

  if [ -n "$label" ]; then
    runtime_dir="$ralph_dir/runtime/$label"
    settings="$runtime_dir/claude-settings.json"
  elif [ -n "${RALPH_RUNTIME_DIR:-}" ] && [ -f "$RALPH_RUNTIME_DIR/claude-settings.json" ]; then
    runtime_dir="$RALPH_RUNTIME_DIR"
    settings="$RALPH_RUNTIME_DIR/claude-settings.json"
  else
    local -a matches=()
    local runtime_root="$ralph_dir/runtime"
    if [ -d "$runtime_root" ]; then
      local entry
      for entry in "$runtime_root"/*/claude-settings.json; do
        [ -f "$entry" ] && matches+=("$entry")
      done
    fi
    if [ "${#matches[@]}" -eq 1 ]; then
      settings="${matches[0]}"
      runtime_dir="$(dirname "$settings")"
    elif [ "${#matches[@]}" -gt 1 ]; then
      echo "simulate_compact_event: multiple runtime settings found under $runtime_root; pass a label" >&2
      return 1
    fi
  fi

  if [ -z "$settings" ] || [ ! -f "$settings" ]; then
    echo "simulate_compact_event: claude-settings.json not found (RALPH_RUNTIME_DIR=${RALPH_RUNTIME_DIR:-unset}, ralph_dir=$ralph_dir)" >&2
    return 1
  fi

  local cmd
  if ! cmd=$(jq -er '
    (.hooks.SessionStart // [])
    | map(select(.matcher == "compact"))
    | .[0].hooks[0].command // empty
  ' "$settings"); then
    echo "simulate_compact_event: failed to parse $settings as JSON" >&2
    return 1
  fi

  if [ -z "$cmd" ]; then
    echo "simulate_compact_event: no SessionStart[compact] hook registered in $settings" >&2
    return 1
  fi

  # install_repin_hook writes a container-view path (/workspace/...); rewrite
  # to the test-tree runtime dir when that file isn't directly executable.
  if [ ! -x "$cmd" ]; then
    local rewritten
    rewritten="$runtime_dir/$(basename "$cmd")"
    if [ -x "$rewritten" ]; then
      cmd="$rewritten"
    fi
  fi

  if [ ! -x "$cmd" ]; then
    echo "simulate_compact_event: hook command not executable: $cmd" >&2
    return 1
  fi

  local output
  if ! output=$("$cmd"); then
    echo "simulate_compact_event: hook script exited non-zero: $cmd" >&2
    return 1
  fi

  local ctx
  if ! ctx=$(jq -er '.hookSpecificOutput.additionalContext' <<<"$output" 2>/dev/null); then
    echo "simulate_compact_event: hook output missing hookSpecificOutput.additionalContext" >&2
    echo "  output: $output" >&2
    return 1
  fi

  printf '%s' "$ctx"
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

  # BEADS_DOLT_AUTO_START must survive teardown — sourced at top of dolt-server.sh.
  unset TEST_DIR BD_DB MOCK_SCENARIO RALPH_DIR RALPH_TEMPLATE_DIR BEADS_DOLT_SERVER_PORT
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

#-----------------------------------------------------------------------------
# Host-Side Mocks (wx-8oz8i)
#
# Overlays mocks for wrapix, git push, beads-push, and the ralph dispatcher so
# ralph check/run's host-side branches run end-to-end without a real container.
# Forces the host-side guard via WRAPIX_CLAUDE_CONFIG=/nonexistent and ensures
# TEST_DIR is on a branch (git init + initial commit) so do_push_gate's
# detached-HEAD check passes.
#
# Each mock records invocations under $MOCK_LOG_DIR as <name>.log. Exit codes
# are overridable per-test via env vars — see below.
#
# Usage: setup_host_mocks [--no-git-init]
#   --no-git-init  Skip the git init + initial commit (for detached-HEAD tests
#                  that want to set HEAD state themselves).
#
# Env vars (read by the mocks, not setup_host_mocks itself):
#   MOCK_WRAPIX_EXIT      — exit code for mock wrapix (default 0)
#   MOCK_WRAPIX_HOOK      — shell command run before mock wrapix exits (e.g.
#                           `bd create ... --labels=spec:foo` to simulate
#                           fix-up work). Runs under bash -c with TEST_DIR as
#                           cwd. Failures are logged but do not abort the mock.
#   MOCK_GIT_PUSH_EXIT    — exit code for intercepted 'git push' (default 0)
#   MOCK_BEADS_PUSH_EXIT  — exit code for mock beads-push (default 0)
#   MOCK_RALPH_DISPATCH   — comma-separated sub-commands to dispatch through
#                           to ralph-<cmd> (default "msg"; pass "" to disable).
#                           Sub-commands not in the list are recorded-only so
#                           tests can assert on 'exec ralph run/check' handoffs
#                           without recursing.
#-----------------------------------------------------------------------------
setup_host_mocks() {
  local do_git_init="true"
  if [ "${1:-}" = "--no-git-init" ]; then
    do_git_init="false"
  fi

  local bin="$TEST_DIR/bin"
  local logdir="$TEST_DIR/mock"
  mkdir -p "$logdir"
  export MOCK_LOG_DIR="$logdir"

  # Mock wrapix: optional hook (to synthesise fix-up beads), then exit.
  cat > "$bin/wrapix" <<'WRAPIX_MOCK'
#!/usr/bin/env bash
echo "wrapix $*" >> "$MOCK_LOG_DIR/wrapix.log"
if [ -n "${MOCK_WRAPIX_HOOK:-}" ]; then
  bash -c "$MOCK_WRAPIX_HOOK" >> "$MOCK_LOG_DIR/wrapix.log" 2>&1 \
    || echo "wrapix_hook_failed exit=$?" >> "$MOCK_LOG_DIR/wrapix.log"
fi
exit "${MOCK_WRAPIX_EXIT:-0}"
WRAPIX_MOCK
  chmod +x "$bin/wrapix"

  # Intercept 'git push' only — everything else falls through to real git.
  local real_git
  real_git=$(readlink -f "$bin/git" 2>/dev/null || command -v git)
  rm -f "$bin/git"
  cat > "$bin/git" <<GIT_MOCK
#!/usr/bin/env bash
if [ "\${1:-}" = "push" ]; then
  echo "git \$*" >> "\$MOCK_LOG_DIR/git.log"
  exit "\${MOCK_GIT_PUSH_EXIT:-0}"
fi
exec "$real_git" "\$@"
GIT_MOCK
  chmod +x "$bin/git"

  cat > "$bin/beads-push" <<'BEADS_MOCK'
#!/usr/bin/env bash
echo "beads-push $*" >> "$MOCK_LOG_DIR/beads-push.log"
exit "${MOCK_BEADS_PUSH_EXIT:-0}"
BEADS_MOCK
  chmod +x "$bin/beads-push"

  # ralph dispatcher: records every call; selectively forwards to ralph-<cmd>
  # for non-recursive sub-commands (default: msg). 'run' / 'check' would recurse
  # into the same host branch, so they default to record-only.
  cat > "$bin/ralph" <<RALPH_MOCK
#!/usr/bin/env bash
echo "ralph \$*" >> "\$MOCK_LOG_DIR/ralph.log"
sub="\${1:-}"
dispatch_list="\${MOCK_RALPH_DISPATCH-msg}"
if [ -n "\$sub" ] && [ -n "\$dispatch_list" ]; then
  IFS=',' read -ra _cmds <<< "\$dispatch_list"
  for _c in "\${_cmds[@]}"; do
    if [ "\$_c" = "\$sub" ] && [ -x "$bin/ralph-\$sub" ]; then
      shift
      exec "$bin/ralph-\$sub" "\$@"
    fi
  done
fi
exit 0
RALPH_MOCK
  chmod +x "$bin/ralph"

  # Force host-side guard branch in ralph scripts.
  export WRAPIX_CLAUDE_CONFIG="/nonexistent"

  # do_push_gate refuses on detached HEAD — give tests a real branch.
  if [ "$do_git_init" = "true" ] && [ ! -d "$TEST_DIR/.git" ]; then
    (
      cd "$TEST_DIR" && \
      git init -q -b main && \
      git -c user.email=test@test -c user.name=Test commit -q --allow-empty -m "init"
    )
  fi
}

# Assert a mock was invoked with an argument string matching the pattern.
# Usage: assert_mock_called <mock_name> <grep_pattern> [message]
assert_mock_called() {
  local name="$1"
  local pattern="$2"
  local msg="${3:-mock $name should have been called with: $pattern}"
  local log="$MOCK_LOG_DIR/$name.log"

  if [ -f "$log" ] && grep -qE "$pattern" "$log"; then
    test_pass "$msg"
    return 0
  fi

  test_fail "$msg"
  echo "  Expected pattern: $pattern"
  echo "  Log ($log):"
  if [ -f "$log" ]; then
    sed 's/^/    /' "$log"
  else
    echo "    (missing)"
  fi
  return 1
}

# Assert a mock was NOT invoked at all (log absent or empty).
# Usage: assert_mock_not_called <mock_name> [message]
assert_mock_not_called() {
  local name="$1"
  local msg="${2:-mock $name should not have been called}"
  local log="$MOCK_LOG_DIR/$name.log"

  if [ ! -s "$log" ]; then
    test_pass "$msg"
    return 0
  fi

  test_fail "$msg"
  echo "  Log ($log):"
  sed 's/^/    /' "$log"
  return 1
}
