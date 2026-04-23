#!/usr/bin/env bash
set -euo pipefail

# ralph run [--once|-1] [--check|-c] [--spec <name>|-s <name>]
# Execute work items for a feature
#
# Modes:
#   Default: Loop through all work items until done (replaces loop.sh)
#   --once/-1: Execute single issue then exit (replaces step.sh)
#
# Options:
#   --spec/-s <name>: Operate on named spec (default: current spec from state/current)
#   --check/-c: Auto-trigger post-epic review after molecule reaches 100%
#
# Spec resolution: reads the spec label ONCE at startup (from --spec flag or
# state/current). The label is held in memory for the duration of the run —
# switching state/current via 'ralph use' does not affect a running 'ralph run'.
# Does NOT update state/current during execution.
#
# Each iteration runs with fresh context (new claude process).
# When all beads complete, transitions WIP -> REVIEW.
#
# SH-6 convention: jq lookups of config/state/bd JSON below use
# `2>/dev/null || <default>` as best-effort: missing/malformed inputs fall
# back to sensible defaults (empty string, "0", "block", numeric limits) so
# the loop continues rather than aborts. `bd dolt commit || true` during the
# loop tolerates "nothing to commit" between iterations; the final push path
# surfaces errors explicitly.

# Parse flags (including --spec/-s early, before container detection)
RUN_ONCE=false
RUN_CHECK=false
PROFILE_OVERRIDE=""
SPEC_FLAG=""
PARALLEL_FLAG=""
RUN_ARGS=()

for arg in "$@"; do
  if [ "${_next_is_spec:-}" = "1" ]; then
    SPEC_FLAG="$arg"
    unset _next_is_spec
    continue
  fi
  if [ "${_next_is_parallel:-}" = "1" ]; then
    PARALLEL_FLAG="$arg"
    unset _next_is_parallel
    continue
  fi
  case "$arg" in
    --once|-1)
      RUN_ONCE=true
      ;;
    --check|-c)
      RUN_CHECK=true
      ;;
    --profile=*)
      PROFILE_OVERRIDE="${arg#--profile=}"
      ;;
    --spec|-s)
      _next_is_spec=1
      ;;
    --spec=*)
      SPEC_FLAG="${arg#--spec=}"
      ;;
    --parallel|-p)
      _next_is_parallel=1
      ;;
    --parallel=*)
      PARALLEL_FLAG="${arg#--parallel=}"
      ;;
    *)
      RUN_ARGS+=("$arg")
      ;;
  esac
done
unset _next_is_spec
unset _next_is_parallel

# Replace positional params with filtered args
set -- "${RUN_ARGS[@]+"${RUN_ARGS[@]}"}"

# Container detection: if not in container and wrapix is available, re-launch in container
# /etc/wrapix/claude-config.json only exists inside containers (baked into image)
if [ ! -f /etc/wrapix/claude-config.json ] && command -v wrapix &>/dev/null; then
  export RALPH_MODE=1
  export RALPH_CMD=run
  # Preserve --once and --spec flags in args for container re-exec
  RALPH_ARGS_PARTS=""
  if [ "$RUN_ONCE" = "true" ]; then
    RALPH_ARGS_PARTS="--once"
  fi
  if [ "$RUN_CHECK" = "true" ]; then
    RALPH_ARGS_PARTS="${RALPH_ARGS_PARTS:+$RALPH_ARGS_PARTS }--check"
  fi
  if [ -n "$SPEC_FLAG" ]; then
    RALPH_ARGS_PARTS="${RALPH_ARGS_PARTS:+$RALPH_ARGS_PARTS }--spec $SPEC_FLAG"
  fi
  if [ -n "$PARALLEL_FLAG" ]; then
    RALPH_ARGS_PARTS="${RALPH_ARGS_PARTS:+$RALPH_ARGS_PARTS }--parallel $PARALLEL_FLAG"
  fi
  export RALPH_ARGS="${RALPH_ARGS_PARTS:+$RALPH_ARGS_PARTS }${*:-}"

  if [[ -n "$PROFILE_OVERRIDE" ]]; then
    echo "Warning: --profile flag is deprecated; profile is set at build time via WRAPIX_PROFILE" >&2
  fi

  # WRAPIX_PROFILE is set by mkRalph's shellHook/app definition.
  # The profile is baked into the sandbox image, not the binary name —
  # wrapix is always the correct command.
  if [[ -z "${WRAPIX_PROFILE:-}" ]]; then
    echo "Warning: WRAPIX_PROFILE not set, defaulting to base" >&2
  fi

  # shellcheck source=util.sh
  source "$(dirname "$0")/util.sh"

  _host_ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  _host_label="$SPEC_FLAG"
  if [ -z "$_host_label" ] && [ -f "$_host_ralph_dir/state/current" ]; then
    _host_label=$(<"$_host_ralph_dir/state/current")
    _host_label="${_host_label#"${_host_label%%[![:space:]]*}"}"
    _host_label="${_host_label%"${_host_label##*[![:space:]]}"}"
  fi

  # Compaction re-pin hook: only register for --once (single-session runs).
  # Continuous mode spawns many claude sessions across distinct issues, so a
  # statically-written re-pin can't name a single issue without going stale.
  if [ "$RUN_ONCE" = "true" ] && [ -n "$_host_label" ]; then
    _host_state_file="$_host_ralph_dir/state/${_host_label}.json"
    _host_spec="specs/${_host_label}.md"
    [ -f "$_host_ralph_dir/state/${_host_label}.md" ] && _host_spec="$_host_ralph_dir/state/${_host_label}.md"

    _host_companions=""
    if [ -f "$_host_state_file" ]; then
      _host_companions=$(jq -r '(.companions // []) | join(",")' "$_host_state_file" 2>/dev/null || true)
    fi

    # Best-effort: peek at the next ready bead so the re-pin names it.
    # If bd can't reach the backend here, the hook still installs with
    # label/spec and the in-container ralph will resolve the issue anyway.
    _host_issue=""
    _host_title=""
    if command -v bd >/dev/null 2>&1; then
      _next=$(bd ready --label "spec:${_host_label}" --sort priority --json 2>/dev/null \
        | jq -r '[.[] | select(.issue_type != "epic")][0] | "\(.id // "")\t\(.title // "")"' 2>/dev/null || true)
      _host_issue="${_next%%$'\t'*}"
      _host_title="${_next#*$'\t'}"
      [ "$_host_title" = "$_next" ] && _host_title=""
    fi

    _repin_content=$(build_repin_content "$_host_label" run \
      "spec=$_host_spec" \
      "issue=$_host_issue" \
      "title=$_host_title" \
      "companions=$_host_companions")
    install_repin_hook "$_host_label" "$_repin_content"
    # shellcheck disable=SC2064
    trap "rm -rf '$_host_ralph_dir/runtime/$_host_label'" EXIT
  fi

  wrapix
  wrapix_exit=$?
  echo "Syncing beads from container..."
  bd dolt pull || echo "Warning: bd dolt pull failed (beads may not be synced)"

  # Continuous mode hands off to ralph check on the HOST so the push gate,
  # verdict display, clarify detection, iteration counter, and notifications
  # all fire. The exec must live here (post-container) rather than at the end
  # of the in-container loop, because `ralph check` invoked inside the
  # container takes the container-side branch and skips all of the above.
  if [ "$RUN_ONCE" != "true" ] && [ "$wrapix_exit" -eq 0 ]; then
    if [ -n "$_host_label" ]; then
      exec ralph check --spec "$_host_label"
    else
      exec ralph check
    fi
  fi

  exit $wrapix_exit
fi

# Load shared helpers
# shellcheck source=util.sh
source "$(dirname "$0")/util.sh"

# Warn early if scripts or templates are stale
check_ralph_staleness

# Pull latest beads state to ensure we have current data
# This is critical - container may have stale data
# Commit first so dolt pull can merge into a clean working set
debug "Pulling beads database..."
# best-effort: "nothing to commit" is fine; pull below still merges
bd dolt commit >/dev/null || true
bd dolt pull >/dev/null || warn "bd dolt pull failed, continuing with local state"

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
CONFIG_FILE="$RALPH_DIR/config.nix"
SPECS_DIR="specs"
PINNED_CONTEXT_FILE=$(get_pinned_context_file)

# Resolve the spec label ONCE at startup using --spec flag or state/current.
# This label is held in a shell variable for the entire run duration —
# switching state/current via 'ralph use' does NOT affect a running 'ralph run'.
FEATURE_NAME=""
MOLECULE_ID=""
SPEC_HIDDEN="false"

if [ -n "$SPEC_FLAG" ]; then
  # Explicit --spec flag: use resolve_spec_label (errors on missing state file)
  FEATURE_NAME=$(resolve_spec_label "$SPEC_FLAG")
else
  # No --spec flag: try resolve_spec_label first, fall back to legacy current.json
  FEATURE_NAME=$(resolve_spec_label "" 2>/dev/null) || true

  if [ -z "$FEATURE_NAME" ]; then
    # Legacy fallback: try reading from current.json
    CURRENT_FILE="$RALPH_DIR/state/current.json"
    if [ -f "$CURRENT_FILE" ]; then
      FEATURE_NAME=$(jq -r '.label // empty' "$CURRENT_FILE" 2>/dev/null || true)
    fi
  fi
fi

# Read state from per-label state file: state/<label>.json
STATE_FILE="$RALPH_DIR/state/${FEATURE_NAME}.json"
if [ -f "$STATE_FILE" ]; then
  MOLECULE_ID=$(jq -r '.molecule // empty' "$STATE_FILE" 2>/dev/null || true)
  # Derive hidden from spec_path (no .hidden field in state JSON)
  if spec_is_hidden "$STATE_FILE"; then
    SPEC_HIDDEN="true"
  else
    SPEC_HIDDEN="false"
  fi
else
  # Legacy fallback: try reading from current.json if per-label state file doesn't exist
  CURRENT_FILE="$RALPH_DIR/state/current.json"
  if [ -f "$CURRENT_FILE" ]; then
    MOLECULE_ID=$(jq -r '.molecule // empty' "$CURRENT_FILE" 2>/dev/null || true)
    if spec_is_hidden "$CURRENT_FILE"; then
      SPEC_HIDDEN="true"
    else
      SPEC_HIDDEN="false"
    fi
  fi
fi

# Validate we have required state
if [ -z "$MOLECULE_ID" ] && [ -z "$FEATURE_NAME" ]; then
  echo "Error: No molecule ID or feature label found." >&2
  echo "Run 'ralph todo' first to create a molecule." >&2
  exit 1
fi

require_file "$CONFIG_FILE" "Ralph config"

# Load config
debug "Loading config from $CONFIG_FILE"
CONFIG=$(nix eval --json --file "$CONFIG_FILE") || error "Failed to evaluate config: $CONFIG_FILE"
if ! validate_json "$CONFIG" "Config"; then
  error "Config file did not produce valid JSON"
fi

# Load hooks from config
HOOK_PRE_LOOP=""
HOOK_PRE_STEP=""
HOOK_POST_STEP=""
HOOK_POST_LOOP=""
HOOKS_ON_FAILURE="block"

HOOK_PRE_LOOP=$(echo "$CONFIG" | jq -r '.hooks."pre-loop" // empty' 2>/dev/null || true)
HOOK_PRE_STEP=$(echo "$CONFIG" | jq -r '.hooks."pre-step" // .loop."pre-hook" // empty' 2>/dev/null || true)
HOOK_POST_STEP=$(echo "$CONFIG" | jq -r '.hooks."post-step" // .loop."post-hook" // empty' 2>/dev/null || true)
HOOK_POST_LOOP=$(echo "$CONFIG" | jq -r '.hooks."post-loop" // empty' 2>/dev/null || true)
HOOKS_ON_FAILURE=$(echo "$CONFIG" | jq -r '."hooks-on-failure" // "block"' 2>/dev/null || echo "block")

debug "Loaded hooks - pre-loop: ${HOOK_PRE_LOOP:-(none)}, pre-step: ${HOOK_PRE_STEP:-(none)}"
debug "  post-step: ${HOOK_POST_STEP:-(none)}, post-loop: ${HOOK_POST_LOOP:-(none)}"
debug "hooks-on-failure: $HOOKS_ON_FAILURE"

# Load max-retries config (per bead, default 2)
MAX_RETRIES=$(echo "$CONFIG" | jq -r '.loop."max-retries" // 2' 2>/dev/null || echo "2")
debug "Max retries per bead: $MAX_RETRIES"

# Load parallel config: flag overrides config, default 1
PARALLEL=1
if [ -n "$PARALLEL_FLAG" ]; then
  PARALLEL="$PARALLEL_FLAG"
else
  PARALLEL=$(echo "$CONFIG" | jq -r '.loop.parallel // 1' 2>/dev/null || echo "1")
fi
# Validate parallel is a positive integer
if ! [[ "$PARALLEL" =~ ^[1-9][0-9]*$ ]]; then
  error "Invalid parallel value: $PARALLEL (must be a positive integer)"
fi
debug "Parallel concurrency: $PARALLEL"

# Load max-reviews config (for -c/--check flag, default 2)
MAX_REVIEWS=$(echo "$CONFIG" | jq -r '.loop."max-reviews" // 2' 2>/dev/null || echo "2")
debug "Max review cycles: $MAX_REVIEWS"

# Load model override for run phase
MODEL_RUN=$(resolve_model "run" "$CONFIG")
if [ -n "$MODEL_RUN" ]; then
  debug "Model override for run phase: $MODEL_RUN"
fi

#-----------------------------------------------------------------------------
# Hook Support
#-----------------------------------------------------------------------------

# Run a hook with template variable substitution
# Usage: run_hook "hook_name" "hook_cmd" [issue_id] [step_count] [step_exit_code]
# Returns: 0 on success or if hook is empty, handles failure per HOOKS_ON_FAILURE
run_hook() {
  local hook_name="$1"
  local hook_cmd="$2"
  local issue_id="${3:-}"
  local step_count="${4:-}"
  local step_exit_code="${5:-}"

  # Skip if hook is empty
  [ -z "$hook_cmd" ] && return 0

  debug "Running hook: $hook_name"

  # Template variable substitution (FR4)
  hook_cmd="${hook_cmd//\{\{LABEL\}\}/${FEATURE_NAME:-}}"
  hook_cmd="${hook_cmd//\{\{ISSUE_ID\}\}/${issue_id}}"
  hook_cmd="${hook_cmd//\{\{STEP_COUNT\}\}/${step_count}}"
  hook_cmd="${hook_cmd//\{\{STEP_EXIT_CODE\}\}/${step_exit_code}}"

  debug "Hook command after substitution: $hook_cmd"

  # Execute the hook in a subshell to capture exit status
  # (direct eval would exit the shell if hook contains 'exit N')
  set +e
  (eval "$hook_cmd")
  local hook_exit=$?
  set -e

  if [ $hook_exit -ne 0 ]; then
    case "$HOOKS_ON_FAILURE" in
      block)
        echo "Hook '$hook_name' failed (exit code: $hook_exit). Stopping." >&2
        exit $hook_exit
        ;;
      warn)
        warn "Hook '$hook_name' failed (exit code: $hook_exit), continuing..."
        ;;
      skip)
        # Silently continue
        debug "Hook '$hook_name' failed (exit code: $hook_exit), skipping"
        ;;
      *)
        # Unknown mode, treat as block
        echo "Hook '$hook_name' failed (exit code: $hook_exit). Stopping." >&2
        exit $hook_exit
        ;;
    esac
  fi

  return 0
}

#-----------------------------------------------------------------------------
# Completion Helpers
#-----------------------------------------------------------------------------

# Update spec status to REVIEW in the pinned-context file
update_spec_status_to_review() {
  local feature="$1"
  local hidden="$2"

  echo ""
  echo "All tasks for '$feature' are complete!"

  # Only mention spec-index update if not hidden
  if [ "$hidden" != "true" ] && [ -f "$PINNED_CONTEXT_FILE" ]; then
    echo "Please update $PINNED_CONTEXT_FILE to move the spec from WIP to REVIEW."
  fi
}

# Close the epic for this label if it exists and is open
close_epic_if_exists() {
  local label="$1"

  debug "Checking for open epic with label: $label"
  local epic_json
  epic_json=$(bd_json list --label "$label" --json) || {
    warn "Failed to check for epic"
    return 0
  }

  local epic_id
  epic_id=$(echo "$epic_json" | jq -r '[.[] | select(.issue_type == "epic" and (.status == "closed" | not))][0].id // empty' 2>/dev/null)

  if [ -n "$epic_id" ]; then
    echo "Closing epic: $epic_id"
    bd close "$epic_id" --reason="All tasks complete" || warn "Failed to close epic $epic_id"
  fi
}

# Check if all beads are complete
check_all_complete() {
  local label="$1"
  local feature="$2"
  local hidden="$3"

  # Check if any ready beads remain (excluding epics)
  # Note: bd ready (not bd list --ready) applies blocker-aware semantics
  local remaining=0
  local json
  json=$(bd_json ready --label "$label" --json) || {
    warn "Failed to check remaining issues"
    remaining=0
  }

  # Count non-epic work items
  if [ -n "$json" ] && echo "$json" | jq -e 'type == "array"' >/dev/null 2>&1; then
    remaining=$(echo "$json" | jq '[.[] | select(.issue_type == "epic" | not)] | length')
  fi
  debug "Remaining ready work items with label $label: $remaining"

  if [ "$remaining" -eq 0 ]; then
    # Close the epic if all tasks are done
    close_epic_if_exists "$label"
    update_spec_status_to_review "$feature" "$hidden"
  fi
}

#-----------------------------------------------------------------------------
# Core Step Execution
#-----------------------------------------------------------------------------

# Execute a single work item
# Returns:
#   0 - Task completed, more work may remain
#   100 - All work complete
#   1 - Task failed
run_step() {
  local label="$1"
  local hidden="$2"

  local bead_label="spec:$label"
  debug "Looking for issues with label: $bead_label"

  # Find next ready issue with this label (excluding epics - they're containers, not work items)
  # Note: bd ready (not bd list --ready) applies blocker-aware semantics
  local bd_list_json
  bd_list_json=$(bd_json ready --label "$bead_label" --sort priority --json) || {
    warn "bd ready command failed"
    bd_list_json="[]"
  }

  # Drop beads carrying ralph:clarify (waiting on human) and epics (containers)
  local bd_work_items
  bd_work_items=$(filter_clarify_beads "$bd_list_json" \
    | jq '[.[] | select(.issue_type != "epic")]' 2>/dev/null || echo "[]")

  # Note: || true prevents set -e from exiting on empty array (return code 1)
  local next_issue
  next_issue=$(bd_list_first_id "$bd_work_items") || true

  if [ -z "$next_issue" ]; then
    echo "No more ready issues with label: $bead_label"
    echo "All work complete!"

    # Close the epic and transition WIP -> REVIEW
    close_epic_if_exists "$bead_label"
    update_spec_status_to_review "$label" "$hidden"
    # Return 100 signals "all complete"
    return 100
  fi

  echo "Working on: $next_issue"
  bd show "$next_issue"

  # Mark as in-progress
  bd update "$next_issue" --status=in_progress

  # Write bead ID for session audit trail (read by entrypoint on exit)
  echo "$next_issue" > /tmp/wrapix-bead-id

  # Get issue details as JSON for prompt substitution
  debug "Fetching issue details for $next_issue"
  local issue_json
  issue_json=$(bd_json show "$next_issue" --json) || {
    warn "bd show failed for $next_issue"
    issue_json="[]"
  }

  # Parse issue fields
  local issue_title=""
  local issue_desc=""

  if ! validate_json_array "$issue_json" "Issue $next_issue"; then
    warn "Could not parse issue details for $next_issue, continuing with empty values"
  else
    issue_title=$(json_array_field "$issue_json" "title" "Issue")
    issue_desc=$(json_array_field "$issue_json" "description" "Issue")
  fi

  # Warn if critical fields are empty
  if [ -z "$issue_title" ]; then
    warn "Issue $next_issue has no title"
  fi
  debug "Issue title: ${issue_title:0:50}..."

  # Pin context from the configured pinnedContext file
  local pinned_context=""
  if [ -f "$PINNED_CONTEXT_FILE" ]; then
    pinned_context=$(cat "$PINNED_CONTEXT_FILE")
  fi

  # Compute spec path based on hidden flag
  local spec_path
  if [ "$hidden" = "true" ]; then
    spec_path="$RALPH_DIR/state/$label.md"
  else
    spec_path="$SPECS_DIR/$label.md"
  fi

  # Read companion manifests
  local companions=""
  local state_file="$RALPH_DIR/state/${label}.json"
  if [ -f "$state_file" ]; then
    companions=$(read_manifests "$state_file")
  fi

  # Retry loop: attempt up to MAX_RETRIES+1 times (1 initial + MAX_RETRIES retries)
  local attempt=0
  local previous_failure=""

  mkdir -p "$RALPH_DIR/logs"

  while true; do
    # Render template with PREVIOUS_FAILURE context (empty on first attempt)
    local work_prompt
    work_prompt=$(render_template run \
      "SPEC_PATH=$spec_path" \
      "ISSUE_ID=$next_issue" \
      "TITLE=$issue_title" \
      "LABEL=$label" \
      "MOLECULE_ID=$MOLECULE_ID" \
      "COMPANIONS=$companions" \
      "DESCRIPTION=$issue_desc" \
      "PINNED_CONTEXT=$pinned_context" \
      "PREVIOUS_FAILURE=$previous_failure" \
      "EXIT_SIGNALS=")

    local log="$RALPH_DIR/logs/work-$next_issue.log"

    # Run claude with FRESH CONTEXT (new process)
    echo ""
    if [ "$attempt" -gt 0 ]; then
      echo "=== Retry attempt $attempt/$MAX_RETRIES (fresh context) ==="
    else
      echo "=== Starting work (fresh context) ==="
    fi
    echo ""

    # Use stream-json for real-time output display with configurable visibility
    run_claude_stream "$work_prompt" "$log" "$CONFIG" "$MODEL_RUN"

    # Check for completion by examining the result in the JSON log
    if jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_COMPLETE")] | any' -s "$log" >/dev/null 2>&1; then
      echo ""
      echo "Work complete. Closing issue: $next_issue"
      bd close "$next_issue"

      # Push beads to Dolt remote (incremental sync for progress visibility and crash safety)
      # best-effort: "nothing to commit" when no beads changed this step
      bd dolt commit >/dev/null || true
      bd dolt push || echo "Warning: bd dolt push failed"

      # Check if all beads with this label are complete
      check_all_complete "$bead_label" "$label" "$hidden"
      return 0
    elif jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_CLARIFY")] | any' -s "$log" >/dev/null 2>&1; then
      # Agent needs clarification — add ralph:clarify label and store question
      local clarify_text
      clarify_text=$(jq -r 'select(.type == "result") | .result' "$log" \
        | grep -oP 'RALPH_CLARIFY:\s*\K.*' | head -1)

      echo ""
      echo "Agent needs clarification on issue: $next_issue"
      if [ -n "$clarify_text" ]; then
        echo "  Question: $clarify_text"
      fi

      # Add ralph:clarify label (with notification) so ralph run skips this bead
      add_clarify_label "$next_issue" "$clarify_text"

      echo ""
      echo "To answer and unblock:"
      echo "  ralph msg -i $next_issue 'your answer'"
      return 1
    else
      # Work did not complete — check if retries remain
      ((++attempt))

      if [ "$attempt" -gt "$MAX_RETRIES" ]; then
        # Max retries exceeded — label ralph:clarify with failure details and move on
        echo ""
        echo "Work did not complete after $attempt attempt(s). Max retries ($MAX_RETRIES) exceeded."

        local failure_summary
        failure_summary=$(extract_error_from_log "$log")
        add_clarify_label "$next_issue" "Failed after $attempt attempt(s). Last error: $failure_summary"

        echo "Issue $next_issue labeled ralph:clarify — moving to next bead."
        return 1
      fi

      # Extract error context from log for retry
      echo ""
      echo "Work did not complete (attempt $attempt/$((MAX_RETRIES + 1))). Retrying with error context..."

      local error_output
      error_output=$(extract_error_from_log "$log")
      previous_failure="## Previous Failure (attempt $attempt)

The previous attempt to complete this issue failed. Here is the error context from that attempt:

\`\`\`
$error_output
\`\`\`

Please analyze the failure above and take a different approach to complete the task."
    fi
  done
}

#-----------------------------------------------------------------------------
# Parallel Batch Execution
#-----------------------------------------------------------------------------

# Get up to N ready bead IDs for this label (excluding epics and ralph:clarify)
# Usage: get_ready_beads <bead_label> <limit>
# Output: newline-separated bead IDs
get_ready_beads() {
  local bead_label="$1"
  local limit="$2"

  local bd_list_json
  bd_list_json=$(bd_json ready --label "$bead_label" --sort priority --json) || {
    warn "bd ready command failed"
    echo ""
    return 0
  }

  # Drop ralph:clarify beads and epics, then take up to $limit IDs
  filter_clarify_beads "$bd_list_json" \
    | jq -r \
      --argjson limit "$limit" \
      '[.[] | select(.issue_type != "epic")] | .[0:$limit] | .[].id' \
    2>/dev/null || true
}

# Run a single step in a worktree (for parallel dispatch)
# This is a simplified version of run_step that operates in a given worktree
# Usage: run_step_in_worktree <worktree_path> <bead_id> <label> <hidden>
# Returns: 0 on RALPH_COMPLETE, 1 on failure
run_step_in_worktree() {
  local worktree_path="$1"
  local bead_id="$2"
  local label="$3"
  local hidden="$4"

  echo "  [$bead_id] Starting work in worktree: $worktree_path"

  # Mark as in-progress
  bd update "$bead_id" --status=in_progress

  # Get issue details
  local issue_json issue_title issue_desc
  issue_json=$(bd_json show "$bead_id" --json) || issue_json="[]"

  if validate_json_array "$issue_json" "Issue $bead_id" 2>/dev/null; then
    issue_title=$(json_array_field "$issue_json" "title" "Issue")
    issue_desc=$(json_array_field "$issue_json" "description" "Issue")
  else
    issue_title=""
    issue_desc=""
  fi

  # Pin context
  local pinned_context=""
  if [ -f "$PINNED_CONTEXT_FILE" ]; then
    pinned_context=$(cat "$PINNED_CONTEXT_FILE")
  fi

  # Compute spec path
  local spec_path
  if [ "$hidden" = "true" ]; then
    spec_path="$RALPH_DIR/state/$label.md"
  else
    spec_path="$SPECS_DIR/$label.md"
  fi

  # Read companion manifests
  local companions=""
  local state_file="$RALPH_DIR/state/${label}.json"
  if [ -f "$state_file" ]; then
    companions=$(read_manifests "$state_file")
  fi

  # Render template
  local work_prompt
  work_prompt=$(render_template run \
    "SPEC_PATH=$spec_path" \
    "ISSUE_ID=$bead_id" \
    "TITLE=$issue_title" \
    "LABEL=$label" \
    "MOLECULE_ID=$MOLECULE_ID" \
    "COMPANIONS=$companions" \
    "DESCRIPTION=$issue_desc" \
    "PINNED_CONTEXT=$pinned_context" \
    "EXIT_SIGNALS=")

  mkdir -p "$RALPH_DIR/logs"
  local log="$RALPH_DIR/logs/work-$bead_id.log"

  # Run claude in the worktree directory
  echo "  [$bead_id] Starting claude session..."
  (
    cd "$worktree_path"
    run_claude_stream "$work_prompt" "$log" "$CONFIG" "$MODEL_RUN"
  )

  # Check result
  if jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_COMPLETE")] | any' -s "$log" >/dev/null 2>&1; then
    echo "  [$bead_id] Work complete. Closing issue."
    bd close "$bead_id"
    # best-effort: "nothing to commit" possible; push failure retried next step
    bd dolt commit >/dev/null || true
    bd dolt push || warn "bd dolt push failed for $bead_id (retry next step)"
    return 0
  elif jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_CLARIFY")] | any' -s "$log" >/dev/null 2>&1; then
    local clarify_text
    clarify_text=$(jq -r 'select(.type == "result") | .result' "$log" \
      | grep -oP 'RALPH_CLARIFY:\s*\K.*' | head -1)
    echo "  [$bead_id] Agent needs clarification."
    add_clarify_label "$bead_id" "$clarify_text"
    return 1
  else
    echo "  [$bead_id] Work did not complete. Issue remains in-progress."
    return 1
  fi
}

# Execute a parallel batch of beads
# Usage: run_parallel_batch <label> <hidden> <parallel>
# Returns: 0 if work was done, 100 if all complete, 1 on failure
run_parallel_batch() {
  local label="$1"
  local hidden="$2"
  local parallel="$3"
  local bead_label="spec:$label"

  # Get up to N ready beads
  local bead_ids
  bead_ids=$(get_ready_beads "$bead_label" "$parallel")

  if [ -z "$bead_ids" ]; then
    echo "No more ready issues with label: $bead_label"
    echo "All work complete!"
    close_epic_if_exists "$bead_label"
    update_spec_status_to_review "$label" "$hidden"
    return 100
  fi

  local count
  count=$(echo "$bead_ids" | wc -l)
  echo "Dispatching $count parallel workers..."

  # Arrays for tracking workers
  local -a worktree_paths=()
  local -a worker_beads=()
  local -a worker_pids=()

  # Create worktrees and spawn workers
  while IFS= read -r bead_id; do
    [ -z "$bead_id" ] && continue

    local wt_path
    wt_path=$(create_worktree "$label" "$bead_id") || {
      warn "Failed to create worktree for $bead_id, skipping"
      continue
    }

    worktree_paths+=("$wt_path")
    worker_beads+=("$bead_id")

    # Spawn worker in background
    run_step_in_worktree "$wt_path" "$bead_id" "$label" "$hidden" &
    worker_pids+=($!)

    echo "  Spawned worker for $bead_id (PID ${worker_pids[-1]})"
  done <<< "$bead_ids"

  # Wait for all workers to complete
  echo "Waiting for ${#worker_pids[@]} workers..."
  local -a worker_exits=()
  for pid in "${worker_pids[@]}"; do
    local exit_code=0
    wait "$pid" || exit_code=$?
    worker_exits+=("$exit_code")
  done

  # Merge worktrees back sequentially
  echo "Merging worktree branches..."
  local any_success=false
  local merge_failures=0
  for i in "${!worktree_paths[@]}"; do
    local wt="${worktree_paths[$i]}"
    local bead="${worker_beads[$i]}"
    local wexit="${worker_exits[$i]}"

    if [ "$wexit" -eq 0 ]; then
      # Worker succeeded — merge back
      if merge_worktree "$wt" "$bead"; then
        echo "  [$bead] Merged successfully."
        any_success=true
      else
        echo "  [$bead] Merge conflict — bead reopened with ralph:clarify."
        merge_failures=$((merge_failures + 1))
      fi
    else
      # Worker failed — clean up worktree
      echo "  [$bead] Worker failed (exit $wexit) — cleaning up."
      cleanup_worktree "$wt"
      # Delete the worktree branch
      local branch_name="ralph/${label}/${bead}"
      # best-effort: branch may already be gone (e.g. never committed)
      git branch -D "$branch_name" || true
    fi
  done

  # Check if all beads with this label are complete
  check_all_complete "$bead_label" "$label" "$hidden"

  if [ "$any_success" = "true" ]; then
    return 0
  else
    return 1
  fi
}

#-----------------------------------------------------------------------------
# Main Execution
#-----------------------------------------------------------------------------

if [ "$RUN_ONCE" = "true" ]; then
  echo "Ralph Wiggum executing single step..."
else
  echo "Ralph Wiggum work loop starting..."
fi

if [ -n "$FEATURE_NAME" ]; then
  echo "  Feature: $FEATURE_NAME"
fi
if [ -n "$MOLECULE_ID" ]; then
  echo "  Molecule: $MOLECULE_ID"
fi
if [ "$PARALLEL" -gt 1 ]; then
  echo "  Parallel: $PARALLEL workers"
fi
if [ "$RUN_CHECK" = "true" ]; then
  echo "  Review: enabled (max $MAX_REVIEWS cycles)"
fi
echo ""

# Run pre-loop hook (even in --once mode, for consistency)
run_hook "pre-loop" "$HOOK_PRE_LOOP"

step_count=0
current_issue_id=""
FINAL_EXIT_CODE=0

while true; do
  ((++step_count))

  if [ "$RUN_ONCE" != "true" ]; then
    echo "=== Step $step_count ==="
  fi

  # Get current issue ID for hook variable substitution
  # Query using molecule ID (preferred) or fall back to feature label
  if [ -n "$MOLECULE_ID" ]; then
    # Use bd ready with molecule filter - more direct and accurate
    current_issue_id=$(bd ready --mol "$MOLECULE_ID" --limit 1 --sort priority 2>/dev/null | \
      grep -oE '^[a-z]+-[a-zA-Z0-9.]+' | head -1 || true)
  elif [ -n "$FEATURE_NAME" ]; then
    # Fall back to label-based query for backward compatibility
    current_issue_id=$(bd ready --label "spec:$FEATURE_NAME" --sort priority --json 2>/dev/null | \
      jq -r '[.[] | select(.issue_type == "epic" | not)][0].id // empty' 2>/dev/null || true)
  fi

  # Run pre-step hook
  run_hook "pre-step" "$HOOK_PRE_STEP" "$current_issue_id" "$step_count"

  # Execute the step (parallel or sequential)
  set +e
  if [ "$PARALLEL" -gt 1 ]; then
    run_parallel_batch "$FEATURE_NAME" "$SPEC_HIDDEN" "$PARALLEL"
  else
    run_step "$FEATURE_NAME" "$SPEC_HIDDEN"
  fi
  EXIT_CODE=$?
  set -e

  # Run post-step hook (with exit code available)
  run_hook "post-step" "$HOOK_POST_STEP" "$current_issue_id" "$step_count" "$EXIT_CODE"

  case $EXIT_CODE in
    0)
      # Task completed, more work may remain
      if [ "$RUN_ONCE" = "true" ]; then
        # Exit after single step
        break
      fi
      # Continue loop
      ;;
    100)
      # All work complete - exit loop
      # In --once mode, propagate exit code 100 to indicate "no work to do"
      if [ "$RUN_ONCE" = "true" ]; then
        FINAL_EXIT_CODE=100
      fi
      break
      ;;
    *)
      if [ "$RUN_ONCE" = "true" ]; then
        echo ""
        echo "Step failed (exit code: $EXIT_CODE)."
        exit 1
      fi
      # In loop mode, continue to the next bead (failed bead has ralph:clarify)
      echo ""
      echo "Step failed (exit code: $EXIT_CODE). Continuing to next bead..."
      ;;
  esac

  if [ "$RUN_ONCE" != "true" ]; then
    echo ""
    echo "--- Continuing to next step ---"
    echo ""
  fi
done

#-----------------------------------------------------------------------------
# Review Cycle (--check/-c flag)
#-----------------------------------------------------------------------------

if [ "$RUN_CHECK" = "true" ] && [ "$FINAL_EXIT_CODE" -eq 0 ] && [ "$RUN_ONCE" != "true" ]; then
  review_cycle=0
  bead_label="spec:$FEATURE_NAME"

  while [ "$review_cycle" -lt "$MAX_REVIEWS" ]; do
    ((++review_cycle))
    echo ""
    echo "=== Review cycle $review_cycle/$MAX_REVIEWS ==="
    echo ""

    # Count beads BEFORE review
    beads_before_json=$(bd_json list --label "$bead_label" --json 2>/dev/null || echo "[]")
    beads_before=$(echo "$beads_before_json" | jq 'length' 2>/dev/null || echo "0")
    debug "Beads before review: $beads_before"

    # Trigger ralph check -s <label>
    set +e
    "$(dirname "$0")/check.sh" -s "$FEATURE_NAME"
    check_exit=$?
    set -e

    # Handle RALPH_CLARIFY from reviewer: check if check.sh indicated clarify
    # check.sh returns 1 on failure — check log for RALPH_CLARIFY
    if [ $check_exit -ne 0 ]; then
      local_check_log="$RALPH_DIR/logs/check-${FEATURE_NAME}.log"
      if [ -f "$local_check_log" ] && jq -e '[.[] | select(.type == "result") | .result | contains("RALPH_CLARIFY")] | any' -s "$local_check_log" >/dev/null 2>&1; then
        # Add ralph:clarify label to the epic bead with the question
        clarify_text=$(jq -r 'select(.type == "result") | .result' "$local_check_log" \
          | grep -oP 'RALPH_CLARIFY:\s*\K.*' | head -1)

        echo "Reviewer needs clarification. Pausing review cycle."

        # Find the epic bead for this label
        epic_json=$(bd_json list --label "$bead_label" --json 2>/dev/null || echo "[]")
        epic_id=$(echo "$epic_json" | jq -r '[.[] | select(.issue_type == "epic" and (.status == "closed" | not))][0].id // empty' 2>/dev/null)

        if [ -n "$epic_id" ]; then
          add_clarify_label "$epic_id" "$clarify_text"
          echo "Epic $epic_id labeled ralph:clarify."
          echo "To respond: ralph msg -i $epic_id 'your answer'"
        fi
        break
      fi
    fi

    # Count beads AFTER review
    beads_after_json=$(bd_json list --label "$bead_label" --json 2>/dev/null || echo "[]")
    beads_after=$(echo "$beads_after_json" | jq 'length' 2>/dev/null || echo "0")
    new_beads=$((beads_after - beads_before))
    debug "Beads after review: $beads_after (new: $new_beads)"

    if [ "$new_beads" -le 0 ]; then
      # Review passed — no new beads
      echo ""
      echo "Review passed for $FEATURE_NAME"
      notify_event "Ralph" "Review passed for $FEATURE_NAME"
      break
    fi

    echo ""
    echo "Review found $new_beads new bead(s). Resuming work loop..."
    notify_event "Ralph" "Review found $new_beads issue(s) for $FEATURE_NAME"
    echo ""

    # Resume work loop to process new beads
    while true; do
      ((++step_count))
      echo "=== Step $step_count (review follow-up) ==="

      current_issue_id=""
      if [ -n "$FEATURE_NAME" ]; then
        current_issue_id=$(bd ready --label "$bead_label" --sort priority --json 2>/dev/null | \
          jq -r '[.[] | select(.issue_type == "epic" | not)][0].id // empty' 2>/dev/null || true)
      fi

      run_hook "pre-step" "$HOOK_PRE_STEP" "$current_issue_id" "$step_count"

      set +e
      if [ "$PARALLEL" -gt 1 ]; then
        run_parallel_batch "$FEATURE_NAME" "$SPEC_HIDDEN" "$PARALLEL"
      else
        run_step "$FEATURE_NAME" "$SPEC_HIDDEN"
      fi
      EXIT_CODE=$?
      set -e

      run_hook "post-step" "$HOOK_POST_STEP" "$current_issue_id" "$step_count" "$EXIT_CODE"

      case $EXIT_CODE in
        100)
          # All follow-up beads complete — re-trigger review
          break
          ;;
        0)
          # Task completed, more may remain
          ;;
        *)
          echo "Step failed (exit code: $EXIT_CODE). Continuing to next bead..."
          ;;
      esac
    done
    # Loop back to re-trigger review
  done

  # Check if max reviews reached
  if [ "$review_cycle" -ge "$MAX_REVIEWS" ]; then
    # Verify the last review didn't pass (i.e., new beads were still created)
    if [ "${new_beads:-0}" -gt 0 ]; then
      echo ""
      echo "Review limit reached for $FEATURE_NAME ($MAX_REVIEWS cycles)"
      notify_event "Ralph" "Review limit reached for $FEATURE_NAME"
    fi
  fi
fi

# Run post-loop hook (even in --once mode, for consistency)
run_hook "post-loop" "$HOOK_POST_LOOP"

echo ""
echo "All work complete after $step_count step(s)!"

# Handoff to ralph check runs on the HOST (see run.sh host-side block),
# not here — an in-container exec lands in check.sh's container-side
# branch and skips the push gate, verdict, and iteration counter.

exit $FINAL_EXIT_CODE
