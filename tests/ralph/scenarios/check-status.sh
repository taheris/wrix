# shellcheck shell=bash
# Check-status scenario - verifies issue status and bd mol current integration
# Used to test that:
# 1. run.sh marks issues as in_progress before work starts
# 2. ralph status uses bd mol current to show position markers
# 3. Position markers correctly show [current], [done], [blocked] states

phase_run() {
  local issue_id="${CHECK_ISSUE_ID:-}"

  if [ -z "$issue_id" ]; then
    echo "ERROR: CHECK_ISSUE_ID not set"
    exit 1
  fi

  # Check the issue status
  local status
  status=$(bd show "$issue_id" --json 2>/dev/null | jq -r '.[0].status // "unknown"' 2>/dev/null || echo "unknown")

  echo "Checking issue status during execution..."
  echo "Issue: $issue_id"
  echo "Status: $status"

  if [ "$status" = "in_progress" ]; then
    echo "STATUS_WAS_IN_PROGRESS"
  else
    echo "STATUS_WAS_NOT_IN_PROGRESS: $status"
  fi

  echo "RALPH_COMPLETE"
}

# Test bd mol current output with position markers
# This phase is used when CHECK_MOL_CURRENT=true
phase_run_mol_current() {
  local molecule_id="${CHECK_MOLECULE_ID:-}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"

  if [ -z "$molecule_id" ]; then
    echo "ERROR: CHECK_MOLECULE_ID not set"
    exit 1
  fi

  echo "Testing bd mol current output..."
  echo "Molecule: $molecule_id"

  # Run bd mol current and capture output
  local mol_current_output
  local mol_current_exit
  set +e
  mol_current_output=$(bd mol current "$molecule_id" 2>&1)
  mol_current_exit=$?
  set -e

  echo "bd mol current exit code: $mol_current_exit"
  echo "Output:"
  echo "$mol_current_output"
  echo ""

  if [ $mol_current_exit -eq 0 ]; then
    echo "MOL_CURRENT_SUCCEEDED"

    # Check for position markers
    if echo "$mol_current_output" | grep -q '\[current\]'; then
      echo "HAS_CURRENT_MARKER"
    fi
    if echo "$mol_current_output" | grep -q '\[done\]'; then
      echo "HAS_DONE_MARKER"
    fi
    if echo "$mol_current_output" | grep -q '\[blocked\]'; then
      echo "HAS_BLOCKED_MARKER"
    fi
    if echo "$mol_current_output" | grep -q '\[ready\]'; then
      echo "HAS_READY_MARKER"
    fi
    if echo "$mol_current_output" | grep -q '\[pending\]'; then
      echo "HAS_PENDING_MARKER"
    fi
  else
    # bd mol current may not support ad-hoc epics
    if echo "$mol_current_output" | grep -qi "not.*molecule\|not.*found\|unknown\|error"; then
      echo "MOL_CURRENT_NOT_SUPPORTED"
    else
      echo "MOL_CURRENT_FAILED"
    fi
  fi

  echo "RALPH_COMPLETE"
}

# Test ralph status output which wraps bd mol commands
phase_status_check() {
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local current_file="$ralph_dir/state/current.json"

  echo "Testing ralph status integration..."

  # Check if current.json has molecule ID
  if [ -f "$current_file" ]; then
    local molecule_id
    molecule_id=$(jq -r '.molecule // empty' "$current_file" 2>/dev/null || true)
    echo "Molecule ID from current.json: ${molecule_id:-not set}"

    if [ -n "$molecule_id" ]; then
      # Test bd mol current directly
      local mol_current_output
      local mol_current_exit
      set +e
      mol_current_output=$(bd mol current "$molecule_id" 2>&1)
      mol_current_exit=$?
      set -e

      echo "bd mol current exit: $mol_current_exit"

      if [ $mol_current_exit -eq 0 ]; then
        echo "MOL_CURRENT_WORKS"
        echo "Position output:"
        echo "$mol_current_output" | head -20
      else
        echo "MOL_CURRENT_UNAVAILABLE"
        echo "Reason: $mol_current_output"
      fi

      # Test bd mol progress
      local mol_progress_output
      local mol_progress_exit
      set +e
      mol_progress_output=$(bd mol progress "$molecule_id" 2>&1)
      mol_progress_exit=$?
      set -e

      echo ""
      echo "bd mol progress exit: $mol_progress_exit"

      if [ $mol_progress_exit -eq 0 ]; then
        echo "MOL_PROGRESS_WORKS"
        echo "Progress output:"
        echo "$mol_progress_output" | head -10
      else
        echo "MOL_PROGRESS_UNAVAILABLE"
      fi
    fi
  else
    echo "No current.json found"
  fi

  echo "RALPH_COMPLETE"
}

phase_plan() {
  echo "RALPH_COMPLETE"
}

phase_todo() {
  local label="${LABEL:-check-status-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"

  # Create a molecule (epic as root)
  local epic_json
  epic_json=$(bd create --title="Check Status Test" --type=epic --labels="spec:$label" --json 2>/dev/null)
  local epic_id
  epic_id=$(echo "$epic_json" | jq -r '.id')

  echo "Created molecule root (epic): $epic_id"

  # Store molecule ID in current.json
  local current_file="$ralph_dir/state/current.json"
  if [ -f "$current_file" ]; then
    local updated_json
    updated_json=$(jq --arg mol "$epic_id" '. + {molecule: $mol}' "$current_file")
    echo "$updated_json" > "$current_file"
    echo "Stored molecule ID: $epic_id"
  fi

  # Create tasks with various states for testing
  # Task A: Will be completed (for [done] marker)
  local task_a_json
  task_a_json=$(bd create --title="Task A - Completed" --type=task --labels="spec:$label" --json 2>/dev/null)
  local task_a_id
  task_a_id=$(echo "$task_a_json" | jq -r '.id')

  # Task B: Will be in_progress (for [current] marker)
  local task_b_json
  task_b_json=$(bd create --title="Task B - In Progress" --type=task --labels="spec:$label" --json 2>/dev/null)
  local task_b_id
  task_b_id=$(echo "$task_b_json" | jq -r '.id')

  # Task C: Depends on Task B (for [blocked] marker)
  local task_c_json
  task_c_json=$(bd create --title="Task C - Blocked" --type=task --labels="spec:$label" --json 2>/dev/null)
  local task_c_id
  task_c_id=$(echo "$task_c_json" | jq -r '.id')

  # Set up task states
  bd close "$task_a_id" 2>/dev/null || true
  bd update "$task_b_id" --status=in_progress 2>/dev/null || true

  # Add dependency: Task C depends on Task B
  bd dep add "$task_c_id" "$task_b_id" 2>/dev/null || true

  echo ""
  echo "Created tasks:"
  echo "  [done]    Task A: $task_a_id (closed)"
  echo "  [current] Task B: $task_b_id (in_progress)"
  echo "  [blocked] Task C: $task_c_id (depends on B)"

  # Export for use in tests
  echo ""
  echo "TASK_A_ID=$task_a_id"
  echo "TASK_B_ID=$task_b_id"
  echo "TASK_C_ID=$task_c_id"
  echo "EPIC_ID=$epic_id"

  echo ""
  echo "RALPH_COMPLETE"
}
