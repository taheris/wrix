#!/usr/bin/env bash
# Parallel-check scenario - verifies parallel agent task selection
# Used to test that run correctly excludes in_progress and blocked items
#
# This scenario simulates what a second agent would see when checking for work:
# - Items already in_progress should not be selected
# - Items blocked by in_progress dependencies should not be selected
# - Only unblocked, non-in_progress items should be available

phase_run() {
  # First, output what work items are available to a "second agent"
  # This simulates checking bd ready from a parallel agent's perspective

  local label="${PARALLEL_CHECK_LABEL:-spec:test-feature}"

  echo "=== Parallel Agent Simulation ==="
  echo "Checking available work for second agent..."

  # Get all items with our label
  local all_items
  all_items=$(bd list --label "$label" --json 2>/dev/null || echo "[]")

  # Get ready items (should exclude in_progress and blocked)
  local ready_items
  ready_items=$(bd list --label "$label" --ready --json 2>/dev/null || echo "[]")

  # Extract just the IDs and statuses for verification
  echo "All items with label $label:"
  echo "$all_items" | jq -r '.[] | "  \(.id): status=\(.status), type=\(.issue_type)"' 2>/dev/null || echo "  (none)"

  echo ""
  echo "Ready items (excluding in_progress and blocked):"
  echo "$ready_items" | jq -r '.[] | select(.issue_type != "epic") | "  \(.id): \(.title)"' 2>/dev/null || echo "  (none)"

  # Count ready non-epic items
  local ready_count
  ready_count=$(echo "$ready_items" | jq '[.[] | select(.issue_type != "epic")] | length' 2>/dev/null || echo "0")

  echo ""
  echo "READY_WORK_COUNT=$ready_count"

  # Check for specific conditions requested by the test
  if [ -n "${EXPECT_TASK_AVAILABLE:-}" ]; then
    if echo "$ready_items" | jq -e ".[] | select(.id == \"$EXPECT_TASK_AVAILABLE\")" >/dev/null 2>&1; then
      echo "EXPECTED_TASK_AVAILABLE"
    else
      echo "EXPECTED_TASK_NOT_AVAILABLE"
    fi
  fi

  if [ -n "${EXPECT_TASK_NOT_AVAILABLE:-}" ]; then
    if echo "$ready_items" | jq -e ".[] | select(.id == \"$EXPECT_TASK_NOT_AVAILABLE\")" >/dev/null 2>&1; then
      echo "BLOCKED_TASK_WAS_AVAILABLE"
    else
      echo "BLOCKED_TASK_CORRECTLY_EXCLUDED"
    fi
  fi

  echo ""
  echo "RALPH_COMPLETE"
}

phase_plan() {
  echo "RALPH_COMPLETE"
}

phase_todo() {
  echo "RALPH_COMPLETE"
}
