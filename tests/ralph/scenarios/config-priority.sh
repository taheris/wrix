# shellcheck shell=bash
# Config priority scenario - creates issues with environment-specified priority
# Used to test beads.priority configuration
# The test environment sets MOCK_PRIORITY which this scenario reads

phase_plan() {
  # Create a simple spec
  local spec_path="${SPEC_PATH:-specs/${LABEL:-test}.md}"
  mkdir -p "$(dirname "$spec_path")"
  cat > "$spec_path" << 'SPEC_EOF'
# Priority Test Feature

A test feature for verifying priority configuration.

## Requirements

- Task 1: First task
SPEC_EOF
  echo "Created spec at $spec_path"
  echo "RALPH_COMPLETE"
}

phase_todo() {
  local label="${LABEL:-test}"
  # Read priority from environment (set by test), default to 2
  local priority="${MOCK_PRIORITY:-2}"

  # Create a task with the specified priority
  local task_json
  task_json=$(bd create --title="Priority test task" --type=task --labels="spec:$label" --priority="$priority" --json 2>/dev/null)
  local task_id
  task_id=$(echo "$task_json" | jq -r '.id')

  echo "Created task with priority $priority: $task_id"
  echo "RALPH_COMPLETE"
}

phase_run() {
  echo "Implementing task..."
  echo "RALPH_COMPLETE"
}
