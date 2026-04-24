#!/usr/bin/env bash
# Test runner infrastructure for ralph integration tests
# Provides parallel and sequential test execution

#-----------------------------------------------------------------------------
# Color Setup
#-----------------------------------------------------------------------------

# Initialize colors (disabled if not a tty)
setup_colors() {
  if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    CYAN='\033[0;36m'
    NC='\033[0m'
  else
    RED=''
    GREEN=''
    YELLOW=''
    CYAN=''
    NC=''
  fi
  export RED GREEN YELLOW CYAN NC
}

#-----------------------------------------------------------------------------
# Test State
#-----------------------------------------------------------------------------

# Initialize test counters
init_test_state() {
  PASSED=0
  FAILED=0
  SKIPPED=0
  NOT_IMPLEMENTED=0
  FAILED_TESTS=()
  export PASSED FAILED SKIPPED NOT_IMPLEMENTED FAILED_TESTS
}

#-----------------------------------------------------------------------------
# Isolated Test Execution
#-----------------------------------------------------------------------------

# Run a single test in isolation and write results to file
# Usage: run_test_isolated <test_func> <result_file> <output_file>
run_test_isolated() {
  _RTI_TEST_FUNC="$1"
  _RTI_RESULT_FILE="$2"
  _RTI_OUTPUT_FILE="$3"

  # CRITICAL: Disable set -e for the entire function to ensure results are always written
  # This runs in a subshell, so this doesn't affect the parent shell's settings
  set +e

  # Reset counters for this test
  PASSED=0
  FAILED=0
  SKIPPED=0
  NOT_IMPLEMENTED=0
  FAILED_TESTS=()

  # EXIT trap writes results when the subshell exits, even if test_skip (exit 77)
  # or test_not_implemented (exit 78) terminate the subshell early
  # shellcheck disable=SC2329  # invoked via EXIT trap
  _rti_write_results() {
    local exit_code=$?
    # Categorize exit 77/78 if the test function exited directly
    if [ "$exit_code" -eq 77 ]; then
      SKIPPED=1
    elif [ "$exit_code" -eq 78 ]; then
      NOT_IMPLEMENTED=1
    elif [ "$exit_code" -ne 0 ] && [ "$PASSED" -eq 0 ] && [ "$FAILED" -eq 0 ] && [ "$SKIPPED" -eq 0 ]; then
      FAILED=1
      FAILED_TESTS+=("$_RTI_TEST_FUNC: exited with code $exit_code")
    fi
    {
      echo "passed=$PASSED"
      echo "failed=$FAILED"
      echo "skipped=$SKIPPED"
      echo "not_implemented=$NOT_IMPLEMENTED"
      for t in "${FAILED_TESTS[@]}"; do
        echo "failed_test=$t"
      done
    } > "$_RTI_RESULT_FILE"
  }
  trap _rti_write_results EXIT

  # Run the test, capturing output
  # test_skip (exit 77) and test_not_implemented (exit 78) will exit this subshell —
  # the EXIT trap ensures results are always written with correct categorization
  "$_RTI_TEST_FUNC" > "$_RTI_OUTPUT_FILE" 2>&1
}

#-----------------------------------------------------------------------------
# Parallel Test Runner
#-----------------------------------------------------------------------------

# Check if GNU parallel is available
has_parallel() {
  command -v parallel &>/dev/null
}

# Run tests in parallel using background jobs
# Usage: run_tests_parallel <test_array_name>
# Example: run_tests_parallel ALL_TESTS
run_tests_parallel() {
  local -n tests_ref=$1
  local results_dir
  results_dir=$(mktemp -d -t "ralph-test-results-XXXXXX")

  # 16 = host cap (balances CPU/memory against wall time for ~250 tests).
  # 5 = container cap matched to Claude rate limits (heavier concurrency has
  # caused mid-run exits). Set RALPH_TEST_MAX_JOBS=0 for unlimited.
  local default_max_jobs=16
  [ -f /etc/wrapix/claude-config.json ] && default_max_jobs=5
  local max_jobs="${RALPH_TEST_MAX_JOBS-$default_max_jobs}"

  if [ "$max_jobs" -gt 0 ]; then
    echo "Running ${#tests_ref[@]} tests (max $max_jobs concurrent)..."
  else
    echo "Running ${#tests_ref[@]} tests in parallel..."
  fi
  echo ""

  local pids=()
  local test_names=()
  declare -A exit_codes

  # Launch tests, respecting max_jobs limit
  for test_func in "${tests_ref[@]}"; do
    local result_file="$results_dir/${test_func}.result"
    local output_file="$results_dir/${test_func}.output"

    # If we're at the job limit, wait for one to finish
    if [ "$max_jobs" -gt 0 ] && [ "${#pids[@]}" -ge "$max_jobs" ]; then
      # Wait for any job to finish
      wait -n 2>/dev/null || true
      # Clean up finished jobs from our tracking
      local new_pids=()
      local new_names=()
      for i in "${!pids[@]}"; do
        if kill -0 "${pids[$i]}" 2>/dev/null; then
          new_pids+=("${pids[$i]}")
          new_names+=("${test_names[$i]}")
        else
          # Job finished, collect its exit code and show output
          wait "${pids[$i]}" 2>/dev/null && exit_codes[${test_names[$i]}]=0 || exit_codes[${test_names[$i]}]=$?
          local out="$results_dir/${test_names[$i]}.output"
          [ -f "$out" ] && cat "$out"
        fi
      done
      pids=("${new_pids[@]}")
      test_names=("${new_names[@]}")
    fi

    # Launch the test
    # Redirect stdin from /dev/null so no command can block on interactive input
    (set +e; run_test_isolated "$test_func" "$result_file" "$output_file") </dev/null &
    pids+=($!)
    test_names+=("$test_func")
  done

  # Wait for remaining tests to complete
  for i in "${!pids[@]}"; do
    local pid="${pids[$i]}"
    local test_func="${test_names[$i]}"

    if wait "$pid"; then
      exit_codes[$test_func]=0
    else
      exit_codes[$test_func]=$?
    fi

    # Show output
    local output_file="$results_dir/${test_func}.output"
    if [ -f "$output_file" ]; then
      cat "$output_file"
    fi
  done

  # Aggregate results
  local total_passed=0
  local total_failed=0
  local total_skipped=0
  local total_not_implemented=0
  local all_failed_tests=()

  for test_func in "${tests_ref[@]}"; do
    local result_file="$results_dir/${test_func}.result"
    local exit_code="${exit_codes[$test_func]:-1}"

    if [ -f "$result_file" ] && [ -s "$result_file" ]; then
      local p f s ni
      p=$(grep "^passed=" "$result_file" | cut -d= -f2)
      f=$(grep "^failed=" "$result_file" | cut -d= -f2)
      s=$(grep "^skipped=" "$result_file" | cut -d= -f2)
      ni=$(grep "^not_implemented=" "$result_file" | cut -d= -f2 || echo 0)
      total_passed=$((total_passed + p))
      total_failed=$((total_failed + f))
      total_skipped=$((total_skipped + s))
      total_not_implemented=$((total_not_implemented + ni))

      while IFS= read -r line; do
        all_failed_tests+=("${line#failed_test=}")
      done < <(grep "^failed_test=" "$result_file")
    elif [ "$exit_code" -ne 0 ]; then
      # Test subprocess crashed before writing results
      total_failed=$((total_failed + 1))
      all_failed_tests+=("$test_func: CRASHED (exit code $exit_code)")
      echo -e "  ${RED}CRASH${NC}: $test_func (subprocess exited with code $exit_code)"
    fi
  done

  # Clean up
  rm -rf "$results_dir"

  # Summary
  print_test_summary "$total_passed" "$total_failed" "$total_skipped" "$total_not_implemented" "${all_failed_tests[@]}"

  [ "$total_failed" -eq 0 ]
}

#-----------------------------------------------------------------------------
# Sequential Test Runner
#-----------------------------------------------------------------------------

# Run tests sequentially with proper isolation
# Each test runs in a subshell to prevent exit calls from killing the main shell
# Usage: run_tests_sequential <test_array_name>
run_tests_sequential() {
  local -n tests_ref=$1
  local results_dir
  results_dir=$(mktemp -d -t "ralph-test-results-XXXXXX")

  local total_passed=0
  local total_failed=0
  local total_skipped=0
  local total_not_implemented=0
  local all_failed_tests=()

  for test_func in "${tests_ref[@]}"; do
    local result_file="$results_dir/${test_func}.result"
    local output_file="$results_dir/${test_func}.output"

    # Run test in subshell with set +e to prevent early exit from killing the subshell
    # The || exit_code=$? pattern captures the subshell's exit code without triggering set -e
    # Redirect stdin from /dev/null so no command can block on interactive input
    local exit_code=0
    (set +e; run_test_isolated "$test_func" "$result_file" "$output_file") </dev/null || exit_code=$?

    # Show output immediately (sequential mode)
    if [ -f "$output_file" ]; then
      cat "$output_file"
    fi

    # Aggregate results
    if [ -f "$result_file" ] && [ -s "$result_file" ]; then
      local p f s ni
      p=$(grep "^passed=" "$result_file" | cut -d= -f2)
      f=$(grep "^failed=" "$result_file" | cut -d= -f2)
      s=$(grep "^skipped=" "$result_file" | cut -d= -f2)
      ni=$(grep "^not_implemented=" "$result_file" | cut -d= -f2 || echo 0)
      total_passed=$((total_passed + p))
      total_failed=$((total_failed + f))
      total_skipped=$((total_skipped + s))
      total_not_implemented=$((total_not_implemented + ni))

      while IFS= read -r line; do
        all_failed_tests+=("${line#failed_test=}")
      done < <(grep "^failed_test=" "$result_file" || true)
    elif [ "$exit_code" -ne 0 ]; then
      # Test subprocess crashed before writing results
      total_failed=$((total_failed + 1))
      all_failed_tests+=("$test_func: CRASHED (exit code $exit_code)")
      echo -e "  ${RED:-}CRASH${NC:-}: $test_func (subprocess exited with code $exit_code)"
    fi
  done

  # Clean up
  rm -rf "$results_dir"

  # Summary
  print_test_summary "$total_passed" "$total_failed" "$total_skipped" "$total_not_implemented" "${all_failed_tests[@]}"

  [ "$total_failed" -eq 0 ]
}

#-----------------------------------------------------------------------------
# Test Summary
#-----------------------------------------------------------------------------

# Print test summary
# Usage: print_test_summary <passed> <failed> <skipped> <not_implemented> [failed_tests...]
print_test_summary() {
  local passed="$1"
  local failed="$2"
  local skipped="$3"
  local not_implemented="$4"
  shift 4
  local failed_tests=("$@")

  echo ""
  echo "=========================================="
  echo "  Test Summary"
  echo "=========================================="
  echo -e "  ${GREEN}Passed:${NC}           $passed"
  echo -e "  ${RED}Failed:${NC}           $failed"
  echo -e "  ${YELLOW}Skipped:${NC}          $skipped"
  echo -e "  ${YELLOW}Not Implemented:${NC}  $not_implemented"
  echo ""
  echo "Results: $passed passed, $failed failed, $skipped skipped (exit 77), $not_implemented not implemented (exit 78)"
  echo ""

  if [ "$failed" -gt 0 ]; then
    echo -e "${RED}Failed tests:${NC}"
    for t in "${failed_tests[@]}"; do
      echo "  - $t"
    done
    echo ""
  else
    echo -e "${GREEN}All tests passed!${NC}"
  fi
}

#-----------------------------------------------------------------------------
# Prerequisite Checks
#-----------------------------------------------------------------------------

# Check test prerequisites
# Usage: check_prerequisites <mock_claude_path> <scenarios_dir>
check_prerequisites() {
  local mock_claude="$1"
  local scenarios_dir="$2"

  local failed=0

  # Check bd command
  if ! command -v bd &>/dev/null; then
    echo -e "${RED}ERROR: bd command not found${NC}"
    echo "Install beads or ensure it's in PATH"
    failed=1
  fi

  # Check ralph-run command
  if ! command -v ralph-run &>/dev/null; then
    echo -e "${RED}ERROR: ralph-run command not found${NC}"
    echo "Build and install ralph first"
    failed=1
  fi

  # Check mock-claude
  if [ ! -x "$mock_claude" ]; then
    echo -e "${RED}ERROR: mock-claude not found or not executable${NC}"
    echo "Expected at: $mock_claude"
    failed=1
  fi

  # Check scenarios directory
  if [ ! -d "$scenarios_dir" ]; then
    echo -e "${RED}ERROR: scenarios directory not found${NC}"
    echo "Expected at: $scenarios_dir"
    failed=1
  fi

  if [ "$failed" -eq 0 ]; then
    echo "Prerequisites OK"
    return 0
  else
    return 1
  fi
}

#-----------------------------------------------------------------------------
# Main Runner
#-----------------------------------------------------------------------------

# Run all tests with mode selection
# Usage: run_tests <test_array_name> [--sequential]
run_tests() {
  local test_array_name="$1"
  local mode="${2:-parallel}"

  # Check for --sequential flag or RALPH_TEST_SEQUENTIAL env var
  if [ "$mode" = "--sequential" ] || [ "${RALPH_TEST_SEQUENTIAL:-}" = "1" ]; then
    echo "Mode: Sequential"
    echo ""
    run_tests_sequential "$test_array_name"
  else
    echo "Mode: Parallel"
    echo ""
    run_tests_parallel "$test_array_name"
  fi
}
