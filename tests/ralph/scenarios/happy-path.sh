# shellcheck shell=bash
# Happy path scenario - full workflow test
# Tests: ralph plan -> ralph todo -> ralph run
#
# This scenario simulates a complete feature workflow:
# 1. plan: Creates a spec file
# 2. todo: Creates molecule (epic + child tasks), stores molecule ID in current.json
# 3. run --once: Completes the first unblocked task
# 4. run: Completes remaining tasks and closes epic

# State tracking (set by test harness)
# LABEL - feature label (e.g., "test-feature")
# TEST_DIR - test directory root
# RALPH_DIR - ralph directory (typically .wrapix/ralph)

phase_plan() {
  # Create the spec file
  local spec_path="${SPEC_PATH:-specs/${LABEL:-happy-path-test}.md}"

  mkdir -p "$(dirname "$spec_path")"

  cat > "$spec_path" << 'SPEC_EOF'
# Happy Path Feature

A test feature for verifying the full ralph workflow.

## Problem Statement

Need to verify that ralph plan, todo, and run work correctly together.

## Requirements

### Functional

1. **Task A** - First task with no dependencies
2. **Task B** - Second task that depends on Task A
3. **Task C** - Third task that depends on Task A

### Non-Functional

- Tests should be deterministic
- Tests should be fast

## Success Criteria

- [ ] All tasks are completed in dependency order
- [ ] Epic is closed when all tasks complete

## Affected Files

| File | Change |
|------|--------|
| `tests/ralph/scenarios/happy-path.sh` | This test scenario |
SPEC_EOF

  echo "Created spec at $spec_path"
  echo "RALPH_COMPLETE"
}

phase_todo() {
  # Get label from state or environment
  local label="${LABEL:-happy-path-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"

  # Create an epic for this feature (epic becomes the molecule root)
  local epic_json
  epic_json=$(bd create --title="Happy Path Feature" --type=epic --labels="spec:$label" --json 2>/dev/null)
  local epic_id
  epic_id=$(echo "$epic_json" | jq -r '.id')

  echo "Created epic (molecule root): $epic_id"

  # Store molecule ID in current.json
  # The epic is the molecule root per ralph-loop.md spec
  local current_file="$ralph_dir/state/current.json"
  if [ -f "$current_file" ]; then
    # Update existing current.json with molecule ID
    local updated_json
    updated_json=$(jq --arg mol "$epic_id" '. + {molecule: $mol}' "$current_file")
    echo "$updated_json" > "$current_file"
    echo "Stored molecule ID in current.json: $epic_id"
  fi

  # Create tasks and bond them to the molecule using bd mol bond
  # Note: Dependency tests are covered by test_run_respects_dependencies.
  # The happy path test focuses on verifying the full workflow from
  # plan -> todo -> run without dependency complications.
  # Tasks are bonded to the molecule for proper tracking.

  local task_a_id
  task_a_id=$(bd create --title="Task A - First task" --type=task --labels="spec:$label" --silent 2>/dev/null)
  bd mol bond "$epic_id" "$task_a_id" --type parallel 2>/dev/null || true
  echo "Created and bonded Task A: $task_a_id"

  local task_b_id
  task_b_id=$(bd create --title="Task B - Second task" --type=task --labels="spec:$label" --silent 2>/dev/null)
  bd mol bond "$epic_id" "$task_b_id" --type parallel 2>/dev/null || true
  echo "Created and bonded Task B: $task_b_id"

  local task_c_id
  task_c_id=$(bd create --title="Task C - Third task" --type=task --labels="spec:$label" --silent 2>/dev/null)
  bd mol bond "$epic_id" "$task_c_id" --type parallel 2>/dev/null || true
  echo "Created and bonded Task C: $task_c_id"

  echo ""
  echo "Molecule breakdown:"
  echo "  Molecule root (epic): $epic_id (Happy Path Feature)"
  echo "  Task A: $task_a_id"
  echo "  Task B: $task_b_id"
  echo "  Task C: $task_c_id"
  echo ""
  echo "RALPH_COMPLETE"
}

phase_run() {
  # Simulate implementing the current task
  echo "Implementing the assigned task..."
  echo "Reading spec and understanding requirements..."
  echo "Writing code..."
  echo "Running tests..."
  echo "All quality gates passed."
  echo ""
  echo "RALPH_COMPLETE"
}
