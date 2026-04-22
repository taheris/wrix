# shellcheck shell=bash
# Update mode scenario - tests ralph plan --update and ralph todo in update mode
# Verifies:
# 1. ralph plan --update for existing spec works
# 2. ralph todo in update mode bonds new tasks to existing molecule
# 3. Existing tasks are NOT recreated
# 4. New tasks are properly bonded with bd mol bond
#
# Test flow:
# 1. Setup with existing spec + molecule
# 2. Run ralph plan --update
# 3. Run ralph todo (update mode)
# 4. Verify new tasks bonded to existing molecule
# 5. Verify original tasks unchanged

# State tracking (set by test harness)
# LABEL - feature label (e.g., "test-feature")
# TEST_DIR - test directory root
# RALPH_DIR - ralph directory (typically .wrapix/ralph)

phase_plan() {
  # In update mode, we gather NEW requirements and write to state/<label>.md
  # (NOT to the main spec file - that happens in ready phase after merge)
  local label="${LABEL:-update-mode-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local state_file="$ralph_dir/state/$label.md"

  echo "Reviewing existing spec for updates..."
  echo "Discussing new requirements with user..."
  echo "New requirement identified: Add validation feature"
  echo "DEBUG: cwd=$(pwd), state_file=$state_file" >&2

  # Write new requirements to state file (ralph ready will read this)
  mkdir -p "$ralph_dir/state"
  cat > "$state_file" << 'REQEOF'
# New Requirements for update-mode-test

## Requirements

1. **Task D** - Add input validation feature
2. **Task E** - Write validation tests

## Success Criteria

- Input validation catches invalid data before processing
- All validation rules are covered by tests

## Affected Files

| File | Change |
|------|--------|
| `src/validator.sh` | New validation module |
| `tests/validator_test.sh` | Validation tests |
REQEOF

  echo "Wrote new requirements to $state_file"
  echo "RALPH_COMPLETE"
}

phase_todo() {
  # Get label from state or environment
  local label="${LABEL:-update-mode-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local current_file="$ralph_dir/state/current.json"

  # Get existing molecule ID from current.json (set up by test harness)
  local molecule_id
  molecule_id=$(jq -r '.molecule // empty' "$current_file" 2>/dev/null || true)

  if [ -z "$molecule_id" ]; then
    echo "ERROR: No molecule ID found in current.json"
    echo "RALPH_BLOCKED: Missing molecule ID for update mode"
    return
  fi

  echo "Update mode: bonding new tasks to existing molecule"
  echo "Molecule ID: $molecule_id"

  # Create new tasks and bond them to the existing molecule
  # In update mode, we do NOT recreate the epic or existing tasks

  # Create new Task D (new requirement from update)
  local task_d_json
  task_d_json=$(bd create --title="Task D - New validation feature" --type=task --labels="spec:$label" --json 2>/dev/null)
  local task_d_id
  task_d_id=$(echo "$task_d_json" | jq -r '.id')
  echo "Created new Task D: $task_d_id"

  # Bond the new task to the existing molecule
  echo "Bonding Task D to molecule $molecule_id..."
  bd mol bond "$molecule_id" "$task_d_id" --type sequential 2>/dev/null || {
    echo "NOTE: bd mol bond may not be fully implemented"
    echo "Simulating bond by adding dependency/relationship"
  }

  # Create another new task that depends on Task D
  local task_e_json
  task_e_json=$(bd create --title="Task E - Validation tests" --type=task --labels="spec:$label" --json 2>/dev/null)
  local task_e_id
  task_e_id=$(echo "$task_e_json" | jq -r '.id')
  echo "Created new Task E: $task_e_id"

  # Add dependency: Task E depends on Task D
  bd dep add "$task_e_id" "$task_d_id" 2>/dev/null
  echo "Added dependency: Task E depends on Task D"

  # Bond Task E to the molecule
  bd mol bond "$molecule_id" "$task_e_id" --type sequential 2>/dev/null || true

  echo ""
  echo "Update mode complete:"
  echo "  Existing molecule: $molecule_id"
  echo "  New Task D: $task_d_id"
  echo "  New Task E: $task_e_id (depends on D)"
  echo ""
  echo "RALPH_COMPLETE"
}

phase_run() {
  # Standard run implementation
  echo "Implementing the assigned task..."
  echo "Reading spec and understanding requirements..."
  echo "Writing code..."
  echo "Running tests..."
  echo "All quality gates passed."
  echo ""
  echo "RALPH_COMPLETE"
}
