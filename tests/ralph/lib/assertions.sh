#!/usr/bin/env bash
# Test assertion functions for ralph integration tests
# shellcheck disable=SC2034  # CURRENT_TEST may be set externally

#-----------------------------------------------------------------------------
# Core Test Output Functions
#-----------------------------------------------------------------------------

# Print test header
test_header() {
  echo ""
  echo -e "${CYAN:-}=== Test: $1 ===${NC:-}"
}

# Print pass result
test_pass() {
  echo -e "  ${GREEN:-}PASS${NC:-}: $1"
  ((PASSED++)) || true
}

# Print fail result
test_fail() {
  echo -e "  ${RED:-}FAIL${NC:-}: $1"
  ((FAILED++)) || true
  FAILED_TESTS+=("${CURRENT_TEST:-unknown}: $1")
}

# Print skip result and exit test subshell with code 77
test_skip() {
  echo -e "  ${YELLOW:-}SKIP${NC:-}: $1" >&2
  exit 77
}

# Print not-implemented result and exit test subshell with code 78
test_not_implemented() {
  echo -e "  ${YELLOW:-}NOT_IMPL${NC:-}: $1" >&2
  exit 78
}

#-----------------------------------------------------------------------------
# File Assertions
#-----------------------------------------------------------------------------

# Assert file exists
# Usage: assert_file_exists <file> [message]
assert_file_exists() {
  local file="$1"
  local msg="${2:-File should exist: $file}"
  if [ -f "$file" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (file not found: $file)"
  fi
}

# Assert file does not exist
# Usage: assert_file_not_exists <file> [message]
assert_file_not_exists() {
  local file="$1"
  local msg="${2:-File should not exist: $file}"
  if [ ! -f "$file" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (file found: $file)"
  fi
}

# Assert file contains string
# Usage: assert_file_contains <file> <pattern> [message]
assert_file_contains() {
  local file="$1"
  local pattern="$2"
  local msg="${3:-File should contain: $pattern}"
  if [ -f "$file" ] && grep -q "$pattern" "$file"; then
    test_pass "$msg"
  else
    test_fail "$msg (pattern not found in $file)"
  fi
}

#-----------------------------------------------------------------------------
# Exit Code Assertions
#-----------------------------------------------------------------------------

# Assert exit code
# Usage: assert_exit_code <expected> <actual> [message]
assert_exit_code() {
  local expected="$1"
  local actual="$2"
  local msg="${3:-Exit code should be $expected}"
  if [ "$expected" -eq "$actual" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (got $actual)"
  fi
}

#-----------------------------------------------------------------------------
# Beads Assertions
#-----------------------------------------------------------------------------

# Assert beads issue exists with label
# Usage: assert_bead_exists <label> [message]
assert_bead_exists() {
  local label="$1"
  local msg="${2:-Bead with label $label should exist}"
  if bd list --label "$label" --json 2>/dev/null | jq -e 'length > 0' >/dev/null 2>&1; then
    test_pass "$msg"
  else
    test_fail "$msg"
  fi
}

# Assert beads issue count
# Usage: assert_bead_count <label> <expected> [message]
assert_bead_count() {
  local label="$1"
  local expected="$2"
  local msg="${3:-Should have $expected beads with label $label}"
  local actual
  actual=$(bd list --label "$label" --json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
  if [ "$expected" -eq "$actual" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (got $actual)"
  fi
}

# Assert beads issue is closed
# Usage: assert_bead_closed <issue_id> [message]
assert_bead_closed() {
  local issue_id="$1"
  local msg="${2:-Issue $issue_id should be closed}"
  local status
  status=$(bd show "$issue_id" --json 2>/dev/null | jq -r '.[0].status // "unknown"' 2>/dev/null || echo "unknown")
  if [ "$status" = "closed" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (status: $status)"
  fi
}

# Assert beads issue status
# Usage: assert_bead_status <issue_id> <expected> [message]
assert_bead_status() {
  local issue_id="$1"
  local expected="$2"
  local msg="${3:-Issue $issue_id should have status $expected}"
  local actual
  actual=$(bd show "$issue_id" --json 2>/dev/null | jq -r '.[0].status // "unknown"' 2>/dev/null || echo "unknown")
  if [ "$expected" = "$actual" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (got $actual)"
  fi
}

# Assert beads issue has a specific label
# Usage: assert_bead_has_label <issue_id> <label> [message]
assert_bead_has_label() {
  local issue_id="$1"
  local label="$2"
  local msg="${3:-Issue $issue_id should have label $label}"
  local has_label
  has_label=$(bd show "$issue_id" --json 2>/dev/null | jq -r --arg lbl "$label" '.[0].labels // [] | map(select(. == $lbl)) | length' 2>/dev/null || echo "0")
  if [ "$has_label" -gt 0 ]; then
    test_pass "$msg"
  else
    test_fail "$msg"
  fi
}

# Assert beads issue notes contain a string
# Usage: assert_bead_notes_contain <issue_id> <pattern> [message]
assert_bead_notes_contain() {
  local issue_id="$1"
  local pattern="$2"
  local msg="${3:-Issue $issue_id notes should contain: $pattern}"
  local notes
  notes=$(bd show "$issue_id" --json 2>/dev/null | jq -r '.[0].notes // ""' 2>/dev/null || echo "")
  if echo "$notes" | grep -qF "$pattern"; then
    test_pass "$msg"
  else
    test_fail "$msg (notes: ${notes:0:200})"
  fi
}

# Assert beads issue description contains a string
# Usage: assert_bead_description_contains <issue_id> <pattern> [message]
assert_bead_description_contains() {
  local issue_id="$1"
  local pattern="$2"
  local msg="${3:-Issue $issue_id description should contain: $pattern}"
  local description
  description=$(bd show "$issue_id" --json 2>/dev/null | jq -r '.[0].description // ""' 2>/dev/null || echo "")
  if echo "$description" | grep -qF "$pattern"; then
    test_pass "$msg"
  else
    test_fail "$msg (description: ${description:0:200})"
  fi
}

#-----------------------------------------------------------------------------
# Molecule Test Assertions
#-----------------------------------------------------------------------------

# Assert molecule exists
# Usage: assert_molecule_exists <molecule_id> [message]
assert_molecule_exists() {
  local molecule="$1"
  local msg="${2:-Molecule $molecule should exist}"
  if bd mol show "$molecule" >/dev/null 2>&1; then
    test_pass "$msg"
  else
    test_fail "$msg (molecule not found)"
  fi
}

# Assert molecule progress percentage
# Usage: assert_molecule_progress <molecule_id> <expected_pct> [message]
assert_molecule_progress() {
  local molecule="$1"
  local expected_pct="$2"
  local msg="${3:-Molecule $molecule should be $expected_pct% complete}"

  # Get progress output and extract percentage
  local progress_output
  progress_output=$(bd mol progress "$molecule" 2>/dev/null) || {
    test_fail "$msg (failed to get progress)"
    return
  }

  # Parse percentage from output (format: "80% (8/10)" or similar)
  local actual_pct
  actual_pct=$(echo "$progress_output" | grep -oE '[0-9]+%' | head -1 | tr -d '%' || echo "")

  if [ -z "$actual_pct" ]; then
    # Try JSON format if available
    actual_pct=$(bd mol progress "$molecule" --json 2>/dev/null | jq -r '.percentage // empty' 2>/dev/null || echo "")
  fi

  if [ -z "$actual_pct" ]; then
    test_fail "$msg (could not parse percentage from output)"
    return
  fi

  if [ "$expected_pct" -eq "$actual_pct" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (got $actual_pct%)"
  fi
}

# Assert molecule has expected task count
# Usage: assert_molecule_task_count <molecule_id> <expected_total> [message]
assert_molecule_task_count() {
  local molecule="$1"
  local expected="$2"
  local msg="${3:-Molecule $molecule should have $expected tasks}"

  # Get progress output and extract total count
  local progress_output
  progress_output=$(bd mol progress "$molecule" 2>/dev/null) || {
    test_fail "$msg (failed to get progress)"
    return
  }

  # Parse total from output (format: "80% (8/10)" -> extract 10)
  local actual
  actual=$(echo "$progress_output" | grep -oE '\([0-9]+/[0-9]+\)' | head -1 | sed 's/.*\///' | tr -d ')' || echo "")

  if [ -z "$actual" ]; then
    test_fail "$msg (could not parse task count from output)"
    return
  fi

  if [ "$expected" -eq "$actual" ]; then
    test_pass "$msg"
  else
    test_fail "$msg (got $actual)"
  fi
}

# Assert molecule current position shows expected marker
# Usage: assert_molecule_current_marker <molecule_id> <marker> [message]
# Markers: [done], [current], [ready], [blocked], [pending]
assert_molecule_current_marker() {
  local molecule="$1"
  local marker="$2"
  local msg="${3:-Molecule $molecule should show $marker marker}"

  local current_output
  current_output=$(bd mol current "$molecule" 2>/dev/null) || {
    test_fail "$msg (failed to get current position)"
    return
  }

  if echo "$current_output" | grep -q "\\$marker\\]"; then
    test_pass "$msg"
  else
    test_fail "$msg (marker not found in output)"
  fi
}
