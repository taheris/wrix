# shellcheck shell=bash
# Discovered work scenario - tests bd mol bond during run execution
# Verifies:
# 1. bd mol bond --type sequential during run works
# 2. bd mol bond --type parallel during run works
# 3. Sequential bonds block current task completion
# 4. Parallel bonds are independent
#
# Test flow:
# 1. Setup molecule with tasks
# 2. Run that discovers sequential work
# 3. Verify bond created with sequential type
# 4. Run that discovers parallel work
# 5. Verify bond created with parallel type

# State tracking (set by test harness)
# LABEL - feature label (e.g., "test-feature")
# TEST_DIR - test directory root
# RALPH_DIR - ralph directory (typically .wrapix/ralph)
# MOLECULE_ID - molecule ID to bond new issues to
# DISCOVER_TYPE - "sequential" or "parallel"

phase_plan() {
  # Create the spec file
  local spec_path="${SPEC_PATH:-specs/${LABEL:-discovered-work-test}.md}"

  mkdir -p "$(dirname "$spec_path")"

  cat > "$spec_path" << 'SPEC_EOF'
# Discovered Work Feature

A test feature for verifying discovered work bonding.

## Problem Statement

Need to verify that bd mol bond works correctly during run execution
for both sequential and parallel bond types.

## Requirements

### Functional

1. **Main Task** - A task that discovers additional work during implementation
2. **Sequential Discovery** - Work that must be done before current task can complete
3. **Parallel Discovery** - Work that can be done independently

### Non-Functional

- Tests should be deterministic
- Tests should verify bond types

## Success Criteria

- [ ] Sequential bonds block current task
- [ ] Parallel bonds are independent

## Affected Files

| File | Change |
|------|--------|
| `tests/ralph/scenarios/discovered-work.sh` | This test scenario |
SPEC_EOF

  echo "Created spec at $spec_path"
  echo "RALPH_COMPLETE"
}

phase_todo() {
  # Get label from state or environment
  local label="${LABEL:-discovered-work-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"

  # Create an epic for this feature (epic becomes the molecule root)
  local epic_json
  epic_json=$(bd create --title="Discovered Work Feature" --type=epic --labels="spec:$label" --json 2>/dev/null)
  local epic_id
  epic_id=$(echo "$epic_json" | jq -r '.id')

  echo "Created epic (molecule root): $epic_id"

  # Store molecule ID in current.json
  local current_file="$ralph_dir/state/current.json"
  if [ -f "$current_file" ]; then
    local updated_json
    updated_json=$(jq --arg mol "$epic_id" '. + {molecule: $mol}' "$current_file")
    echo "$updated_json" > "$current_file"
    echo "Stored molecule ID in current.json: $epic_id"
  fi

  # Create the main task that will discover work
  local main_task_id
  main_task_id=$(bd create --title="Main Task - discovers additional work" --type=task --labels="spec:$label" --silent 2>/dev/null)
  bd mol bond "$epic_id" "$main_task_id" --type parallel 2>/dev/null || true
  echo "Created and bonded Main Task: $main_task_id"

  echo ""
  echo "Molecule breakdown:"
  echo "  Molecule root (epic): $epic_id (Discovered Work Feature)"
  echo "  Main Task: $main_task_id"
  echo ""
  echo "RALPH_COMPLETE"
}

phase_run() {
  # Simulate implementing a task that discovers additional work
  local label="${LABEL:-discovered-work-test}"
  local ralph_dir="${RALPH_DIR:-.wrapix/ralph}"
  local current_file="$ralph_dir/state/current.json"

  # Get molecule ID from current.json or environment
  local molecule_id="${MOLECULE_ID:-}"
  if [ -z "$molecule_id" ] && [ -f "$current_file" ]; then
    molecule_id=$(jq -r '.molecule // empty' "$current_file" 2>/dev/null || true)
  fi

  if [ -z "$molecule_id" ]; then
    echo "ERROR: No molecule ID available"
    echo "RALPH_BLOCKED: Missing molecule ID"
    return
  fi

  echo "Implementing the assigned task..."
  echo "Molecule: $molecule_id"

  # Determine which type of discovery to simulate
  local discover_type="${DISCOVER_TYPE:-sequential}"
  echo "Discovery type: $discover_type"

  if [ "$discover_type" = "sequential" ]; then
    # Sequential discovery - work that blocks current task
    echo ""
    echo "=== Discovering sequential work ==="
    echo "Found prerequisite that must be completed first..."

    # Create the discovered task
    local discovered_json
    discovered_json=$(bd create --title="Discovered Sequential Work - prerequisite" --type=task --labels="spec:$label" --json 2>/dev/null)
    local discovered_id
    discovered_id=$(echo "$discovered_json" | jq -r '.id')
    echo "Created discovered task: $discovered_id"

    # Bond with sequential type (blocks current task)
    echo "Bonding with --type sequential..."
    if bd mol bond "$molecule_id" "$discovered_id" --type sequential 2>&1; then
      echo "SEQUENTIAL_BOND_SUCCESS"
      echo "Discovered task $discovered_id bonded sequentially to molecule $molecule_id"
    else
      echo "SEQUENTIAL_BOND_FAILED"
      echo "WARNING: bd mol bond --type sequential may not be fully implemented"
    fi

    # Output the discovered task ID for verification
    echo "DISCOVERED_TASK_ID=$discovered_id"
    echo "BOND_TYPE=sequential"

  elif [ "$discover_type" = "parallel" ]; then
    # Parallel discovery - independent work
    echo ""
    echo "=== Discovering parallel work ==="
    echo "Found related work that can be done independently..."

    # Create the discovered task
    local discovered_json
    discovered_json=$(bd create --title="Discovered Parallel Work - independent" --type=task --labels="spec:$label" --json 2>/dev/null)
    local discovered_id
    discovered_id=$(echo "$discovered_json" | jq -r '.id')
    echo "Created discovered task: $discovered_id"

    # Bond with parallel type (independent work)
    echo "Bonding with --type parallel..."
    if bd mol bond "$molecule_id" "$discovered_id" --type parallel 2>&1; then
      echo "PARALLEL_BOND_SUCCESS"
      echo "Discovered task $discovered_id bonded in parallel to molecule $molecule_id"
    else
      echo "PARALLEL_BOND_FAILED"
      echo "WARNING: bd mol bond --type parallel may not be fully implemented"
    fi

    # Output the discovered task ID for verification
    echo "DISCOVERED_TASK_ID=$discovered_id"
    echo "BOND_TYPE=parallel"

  elif [ "$discover_type" = "both" ]; then
    # Discover both types in one run
    echo ""
    echo "=== Discovering both sequential and parallel work ==="

    # Sequential task
    local seq_json
    seq_json=$(bd create --title="Discovered Sequential Work - must do first" --type=task --labels="spec:$label" --json 2>/dev/null)
    local seq_id
    seq_id=$(echo "$seq_json" | jq -r '.id')
    echo "Created sequential task: $seq_id"

    if bd mol bond "$molecule_id" "$seq_id" --type sequential 2>&1; then
      echo "SEQUENTIAL_BOND_SUCCESS"
    else
      echo "SEQUENTIAL_BOND_FAILED"
    fi

    # Parallel task
    local par_json
    par_json=$(bd create --title="Discovered Parallel Work - can do later" --type=task --labels="spec:$label" --json 2>/dev/null)
    local par_id
    par_id=$(echo "$par_json" | jq -r '.id')
    echo "Created parallel task: $par_id"

    if bd mol bond "$molecule_id" "$par_id" --type parallel 2>&1; then
      echo "PARALLEL_BOND_SUCCESS"
    else
      echo "PARALLEL_BOND_FAILED"
    fi

    echo "SEQUENTIAL_TASK_ID=$seq_id"
    echo "PARALLEL_TASK_ID=$par_id"
  fi

  echo ""
  echo "RALPH_COMPLETE"
}
