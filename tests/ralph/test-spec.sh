#!/usr/bin/env bash
# Integration tests for ralph spec commands
# Tests annotation counting, verbose output, verify/judge runners, --all flag,
# multi-spec iteration, --spec filter, short flags, and grouped output
# shellcheck disable=SC2329,SC2086,SC2034,SC1091
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
MOCK_CLAUDE="$SCRIPT_DIR/mock-claude"
SCENARIOS_DIR="$SCRIPT_DIR/scenarios"
LIB_DIR="$SCRIPT_DIR/lib"

# Source test libraries
source "$LIB_DIR/assertions.sh"
source "$LIB_DIR/fixtures.sh"
source "$LIB_DIR/runner.sh"

init_test_state
setup_colors

#-----------------------------------------------------------------------------
# Helper: create a sample spec with mixed annotations for testing
#-----------------------------------------------------------------------------
create_annotated_spec() {
  local spec_file="$1"
  cat > "$spec_file" << 'SPEC'
# Test Feature

## Requirements

Some requirements.

## Success Criteria

- [ ] Fast response time
  [verify](tests/perf-test.sh::test_response_time)
- [ ] Output is human-readable
  [judge](tests/judges/readability.sh::test_readable_output)
- [ ] Works offline
- [x] Basic API works
  [verify](tests/api.sh::test_basic_api)
- [ ] Handles edge cases gracefully
  [judge](tests/judges/edge.sh::test_edge_cases)
- [ ] No security vulnerabilities

## Out of Scope

Nothing relevant.
SPEC
}

#-----------------------------------------------------------------------------
# Helper: update docs/README.md spec table with a molecule ID for a spec
#-----------------------------------------------------------------------------
add_readme_spec_entry() {
  local spec_name="$1"
  local molecule_id="${2:-}"
  local beads_col="${molecule_id:----}"
  echo "| [${spec_name}.md](../specs/${spec_name}.md) | — | $beads_col | Test spec |" >> "$TEST_DIR/docs/README.md"
}

#-----------------------------------------------------------------------------
# Test: ralph spec produces correct annotation counts
#-----------------------------------------------------------------------------
test_spec_annotation_counts() {
  CURRENT_TEST="spec_annotation_counts"
  test_header "Ralph Spec Annotation Counts"

  setup_test_env "spec-counts"

  # Create spec files with known annotations
  create_annotated_spec "$TEST_DIR/specs/test-feature.md"

  # Create a second spec with different counts
  cat > "$TEST_DIR/specs/other-feature.md" << 'SPEC'
# Other Feature

## Success Criteria

- [ ] Criterion A
- [ ] Criterion B
- [ ] Criterion C
  [verify](tests/c-test.sh::test_c)

## Design

Design section.
SPEC

  local output
  output=$(ralph-spec 2>&1) || true

  # Should list spec files with annotation counts
  if echo "$output" | grep -q "test-feature.md"; then
    test_pass "Output includes test-feature.md"
  else
    test_fail "Output should include test-feature.md"
  fi

  if echo "$output" | grep -q "other-feature.md"; then
    test_pass "Output includes other-feature.md"
  else
    test_fail "Output should include other-feature.md"
  fi

  # test-feature.md has 2 verify, 2 judge, 2 unannotated
  if echo "$output" | grep "test-feature.md" | grep -q "2 verify"; then
    test_pass "test-feature.md shows 2 verify"
  else
    test_fail "test-feature.md should show 2 verify"
  fi

  if echo "$output" | grep "test-feature.md" | grep -q "2 judge"; then
    test_pass "test-feature.md shows 2 judge"
  else
    test_fail "test-feature.md should show 2 judge"
  fi

  if echo "$output" | grep "test-feature.md" | grep -q "2 unannotated"; then
    test_pass "test-feature.md shows 2 unannotated"
  else
    test_fail "test-feature.md should show 2 unannotated"
  fi

  # Should show totals
  if echo "$output" | grep -qi "total"; then
    test_pass "Output includes totals"
  else
    test_fail "Output should include totals"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verbose shows per-criterion detail
#-----------------------------------------------------------------------------
test_spec_verbose() {
  CURRENT_TEST="spec_verbose"
  test_header "Ralph Spec --verbose"

  setup_test_env "spec-verbose"

  create_annotated_spec "$TEST_DIR/specs/test-feature.md"

  local output
  output=$(ralph-spec --verbose 2>&1) || true

  # Should show individual criterion text
  if echo "$output" | grep -q "Fast response time"; then
    test_pass "Shows criterion text: Fast response time"
  else
    test_fail "Should show criterion text: Fast response time"
  fi

  if echo "$output" | grep -q "Output is human-readable"; then
    test_pass "Shows criterion text: Output is human-readable"
  else
    test_fail "Should show criterion text: Output is human-readable"
  fi

  if echo "$output" | grep -q "Works offline"; then
    test_pass "Shows unannotated criterion: Works offline"
  else
    test_fail "Should show unannotated criterion: Works offline"
  fi

  # Should indicate annotation types per criterion with bracketed type
  if echo "$output" | grep -q "\[verify\]"; then
    test_pass "Shows [verify] annotation type"
  else
    test_fail "Should show [verify] annotation type"
  fi

  if echo "$output" | grep -q "\[judge"; then
    test_pass "Shows [judge] annotation type"
  else
    test_fail "Should show [judge] annotation type"
  fi

  if echo "$output" | grep -q "\[none"; then
    test_pass "Shows [none] for unannotated criteria"
  else
    test_fail "Should show [none] for unannotated criteria"
  fi

  # Should show test path with arrow for annotated criteria
  if echo "$output" | grep "Fast response time" | grep -q "→.*tests/perf-test.sh::test_response_time"; then
    test_pass "Shows test path with arrow for verify criterion"
  else
    test_fail "Should show test path with arrow for verify criterion"
  fi

  if echo "$output" | grep "Output is human-readable" | grep -q "→.*tests/judges/readability.sh::test_readable_output"; then
    test_pass "Shows test path with arrow for judge criterion"
  else
    test_fail "Should show test path with arrow for judge criterion"
  fi

  # Unannotated criteria should NOT have arrow/path
  if echo "$output" | grep "Works offline" | grep -qv "→"; then
    test_pass "Unannotated criterion has no arrow/path"
  else
    test_fail "Unannotated criterion should not have arrow/path"
  fi

  # Should still show summary totals at the bottom
  if echo "$output" | grep -qi "total"; then
    test_pass "Verbose output still includes totals"
  else
    test_fail "Verbose output should still include totals"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify runs shell tests and reports PASS/FAIL/SKIP
# Uses --spec to test single-spec format
#-----------------------------------------------------------------------------
test_spec_verify() {
  CURRENT_TEST="spec_verify"
  test_header "Ralph Spec --verify"

  setup_test_env "spec-verify"

  # Create a spec with verify and judge annotations
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test passes
  [verify](tests/pass-test.sh::test_passes)
- [ ] Test fails
  [verify](tests/fail-test.sh::test_fails)
- [ ] Judge only criterion
  [judge](tests/judges/check.sh::test_judge)
- [ ] No annotation
SPEC

  # Create test files: one that passes, one that fails
  mkdir -p "$TEST_DIR/tests/judges"

  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes() {
  echo "All good"
  return 0
}
# If function name passed as arg, call it
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  cat > "$TEST_DIR/tests/fail-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_fails() {
  echo "Something went wrong"
  return 1
}
# If function name passed as arg, call it
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/fail-test.sh"

  local output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  local exit_code=$?
  set -e

  # Should show PASS for passing test
  if echo "$output" | grep -q "\[PASS\]"; then
    test_pass "Shows [PASS] for passing test"
  else
    test_fail "Should show [PASS] for passing test"
  fi

  # Should show FAIL for failing test
  if echo "$output" | grep -q "\[FAIL\]"; then
    test_pass "Shows [FAIL] for failing test"
  else
    test_fail "Should show [FAIL] for failing test"
  fi

  # Should show SKIP for unannotated criterion
  if echo "$output" | grep "No annotation" | grep -q "\[SKIP\]"; then
    test_pass "Shows [SKIP] for unannotated criterion"
  else
    test_fail "Should show [SKIP] for unannotated criterion"
  fi

  # Should show summary (single-spec format)
  if echo "$output" | grep -q "passed"; then
    test_pass "Shows pass count in summary"
  else
    test_fail "Should show pass count in summary"
  fi

  if echo "$output" | grep -q "failed"; then
    test_pass "Shows fail count in summary"
  else
    test_fail "Should show fail count in summary"
  fi

  if echo "$output" | grep -q "skipped"; then
    test_pass "Shows skip count in summary"
  else
    test_fail "Should show skip count in summary"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify reports exit 77 as SKIP and exit 78 as SKIP
#-----------------------------------------------------------------------------
test_spec_verify_skip_exits() {
  CURRENT_TEST="spec_verify_skip_exits"
  test_header "Ralph Spec --verify handles exit 77/78 as SKIP"

  setup_test_env "spec-verify-skip"

  # Create a spec with verify annotations for skip, not-implemented, pass, and fail
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test passes
  [verify](tests/pass-test.sh::test_passes)
- [ ] Test is skipped
  [verify](tests/skip-test.sh::test_skipped)
- [ ] Test is not implemented
  [verify](tests/notimpl-test.sh::test_not_implemented)
- [ ] Test fails
  [verify](tests/fail-test.sh::test_fails)
SPEC

  mkdir -p "$TEST_DIR/tests"

  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  cat > "$TEST_DIR/tests/skip-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_skipped() { exit 77; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/skip-test.sh"

  cat > "$TEST_DIR/tests/notimpl-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_not_implemented() { exit 78; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/notimpl-test.sh"

  cat > "$TEST_DIR/tests/fail-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_fails() { return 1; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/fail-test.sh"

  local output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  local exit_code=$?
  set -e

  # Should show PASS for passing test
  if echo "$output" | grep "Test passes" | grep -q "\[PASS\]"; then
    test_pass "Shows [PASS] for passing test"
  else
    test_fail "Should show [PASS] for passing test"
  fi

  # Should show SKIP for exit 77
  if echo "$output" | grep "Test is skipped" | grep -q "\[SKIP\]"; then
    test_pass "Shows [SKIP] for exit 77 (skipped)"
  else
    test_fail "Should show [SKIP] for exit 77"
  fi

  # Exit 77 should include skip reason
  if echo "$output" | grep -q "exit 77"; then
    test_pass "Shows exit 77 in output"
  else
    test_fail "Should show exit 77 in output"
  fi

  # Should show SKIP for exit 78
  if echo "$output" | grep "Test is not implemented" | grep -q "\[SKIP\]"; then
    test_pass "Shows [SKIP] for exit 78 (not implemented)"
  else
    test_fail "Should show [SKIP] for exit 78"
  fi

  # Exit 78 should include not-implemented reason
  if echo "$output" | grep -q "exit 78"; then
    test_pass "Shows exit 78 in output"
  else
    test_fail "Should show exit 78 in output"
  fi

  # Should show FAIL for failing test (exit 1, not 77/78)
  if echo "$output" | grep "Test fails" | grep -q "\[FAIL\]"; then
    test_pass "Shows [FAIL] for exit 1 (real failure)"
  else
    test_fail "Should show [FAIL] for exit 1"
  fi

  # Summary should count skipped tests
  if echo "$output" | grep -q "1 passed.*1 failed.*2 skipped"; then
    test_pass "Summary shows correct counts (1 pass, 1 fail, 2 skip)"
  else
    test_fail "Summary should show 1 passed, 1 failed, 2 skipped. Got: $(echo "$output" | tail -1)"
  fi

  # Exit code should be non-zero (there's still a real failure)
  if [ "$exit_code" -ne 0 ]; then
    test_pass "Exit code is non-zero due to real failure"
  else
    test_fail "Exit code should be non-zero when there is a real failure"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --judge invokes LLM evaluation (mocked)
#-----------------------------------------------------------------------------
test_spec_judge() {
  CURRENT_TEST="spec_judge"
  test_header "Ralph Spec --judge (Mocked)"

  setup_test_env "spec-judge"

  # Create a spec with judge annotations
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Verify only criterion
  [verify](tests/some-test.sh::test_something)
- [ ] Judge criterion
  [judge](tests/judges/quality.sh::test_quality)
- [ ] No annotation
SPEC

  # Create judge test file with rubric
  mkdir -p "$TEST_DIR/tests/judges"
  cat > "$TEST_DIR/tests/judges/quality.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_quality() {
  judge_files "lib/output.sh"
  judge_criterion "Output is well-formatted and includes all required fields"
}
TESTFILE

  local output
  set +e
  output=$(ralph-spec --judge --spec test-feature 2>&1)
  local exit_code=$?
  set -e

  # Should show SKIP for unannotated criterion
  if echo "$output" | grep "No annotation" | grep -q "\[SKIP\]"; then
    test_pass "Shows [SKIP] for unannotated criterion"
  else
    test_fail "Should show [SKIP] for unannotated criterion"
  fi

  # Judge criterion should not be skipped (should show PASS or FAIL)
  if echo "$output" | grep "Judge criterion" | grep -qv "\[SKIP\]"; then
    test_pass "Judge criterion is not skipped"
  else
    test_fail "Judge criterion should not be skipped"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --all runs both verify and judge
#-----------------------------------------------------------------------------
test_spec_all() {
  CURRENT_TEST="spec_all"
  test_header "Ralph Spec --all"

  setup_test_env "spec-all"

  # Create spec with both verify and judge
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Verify criterion
  [verify](tests/check.sh::test_check)
- [ ] Judge criterion
  [judge](tests/judges/eval.sh::test_eval)
- [ ] No annotation
SPEC

  mkdir -p "$TEST_DIR/tests/judges"
  cat > "$TEST_DIR/tests/check.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_check() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/check.sh"

  cat > "$TEST_DIR/tests/judges/eval.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_eval() {
  judge_files "lib/main.sh"
  judge_criterion "Code is correct"
}
TESTFILE

  local output
  set +e
  output=$(ralph-spec --all --spec test-feature 2>&1)
  local exit_code=$?
  set -e

  # --all should NOT skip verify criteria (they run in verify pass)
  if echo "$output" | grep "Verify criterion" | grep -qv "\[SKIP\]"; then
    test_pass "Verify criterion is not skipped with --all"
  else
    test_fail "Verify criterion should not be skipped with --all"
  fi

  # --all should NOT skip judge criteria (they run in judge pass)
  if echo "$output" | grep "Judge criterion" | grep -qv "\[SKIP\]"; then
    test_pass "Judge criterion is not skipped with --all"
  else
    test_fail "Judge criterion should not be skipped with --all"
  fi

  # Unannotated should still be skipped
  if echo "$output" | grep "No annotation" | grep -q "\[SKIP\]"; then
    test_pass "Unannotated criterion still skipped with --all"
  else
    test_fail "Unannotated criterion should be skipped with --all"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec with no flags is instant (no test execution)
#-----------------------------------------------------------------------------
test_spec_no_execution_default() {
  CURRENT_TEST="spec_no_execution_default"
  test_header "Ralph Spec Default (No Execution)"

  setup_test_env "spec-no-exec"

  # Create a spec with verify annotations pointing to slow tests
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Slow test criterion
  [verify](tests/slow-test.sh::test_slow)
SPEC

  # Create a test file that would take time to execute
  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/slow-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_slow() {
  sleep 10
  return 0
}
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/slow-test.sh"

  # ralph spec (no flags) should return quickly without executing tests
  local start_time end_time elapsed
  start_time=$(date +%s)

  set +e
  ralph-spec >/dev/null 2>&1
  set -e

  end_time=$(date +%s)
  elapsed=$((end_time - start_time))

  # Should finish in under 5 seconds (the test sleeps for 10)
  if [ "$elapsed" -lt 5 ]; then
    test_pass "Default ralph spec completes quickly (${elapsed}s)"
  else
    test_fail "Default ralph spec took too long (${elapsed}s) - may be executing tests"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: parse_spec_annotations counts verify/judge/unannotated correctly
# This tests the util.sh function directly, independent of spec.sh
#-----------------------------------------------------------------------------
test_spec_annotation_counting() {
  CURRENT_TEST="spec_annotation_counting"
  test_header "Spec Annotation Counting via parse_spec_annotations"

  setup_test_env "spec-counting"

  # Source util.sh directly
  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  create_annotated_spec "$TEST_DIR/specs/test-feature.md"

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/test-feature.md")

  # Count annotations by type
  local verify_count judge_count none_count
  verify_count=$(echo "$output" | awk -F'\t' '$2 == "verify"' | wc -l)
  judge_count=$(echo "$output" | awk -F'\t' '$2 == "judge"' | wc -l)
  none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)

  if [ "$verify_count" -eq 2 ]; then
    test_pass "Counts 2 verify annotations"
  else
    test_fail "Expected 2 verify annotations, got $verify_count"
  fi

  if [ "$judge_count" -eq 2 ]; then
    test_pass "Counts 2 judge annotations"
  else
    test_fail "Expected 2 judge annotations, got $judge_count"
  fi

  if [ "$none_count" -eq 2 ]; then
    test_pass "Counts 2 unannotated criteria"
  else
    test_fail "Expected 2 unannotated criteria, got $none_count"
  fi

  # Verify total criterion count
  local total
  total=$(echo "$output" | wc -l)
  if [ "$total" -eq 6 ]; then
    test_pass "Total criterion count is 6"
  else
    test_fail "Expected 6 total criteria, got $total"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: annotation parsing handles spec with only verify annotations
#-----------------------------------------------------------------------------
test_spec_verify_only() {
  CURRENT_TEST="spec_verify_only"
  test_header "Spec with Only Verify Annotations"

  setup_test_env "spec-verify-only"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  cat > "$TEST_DIR/specs/verify-only.md" << 'SPEC'
# Verify-Only Feature

## Success Criteria

- [ ] First check
  [verify](tests/a.sh::test_a)
- [ ] Second check
  [verify](tests/b.sh::test_b)
- [ ] Third check
  [verify](tests/c.sh)
SPEC

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/verify-only.md")

  local verify_count judge_count none_count
  verify_count=$(echo "$output" | awk -F'\t' '$2 == "verify"' | wc -l)
  judge_count=$(echo "$output" | awk -F'\t' '$2 == "judge"' | wc -l)
  none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)

  if [ "$verify_count" -eq 3 ]; then
    test_pass "All 3 annotations are verify"
  else
    test_fail "Expected 3 verify, got $verify_count"
  fi

  if [ "$judge_count" -eq 0 ]; then
    test_pass "No judge annotations"
  else
    test_fail "Expected 0 judge, got $judge_count"
  fi

  if [ "$none_count" -eq 0 ]; then
    test_pass "No unannotated criteria"
  else
    test_fail "Expected 0 unannotated, got $none_count"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: annotation parsing handles spec with only unannotated criteria
#-----------------------------------------------------------------------------
test_spec_all_unannotated() {
  CURRENT_TEST="spec_all_unannotated"
  test_header "Spec with All Unannotated Criteria"

  setup_test_env "spec-unannotated"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  cat > "$TEST_DIR/specs/unannotated.md" << 'SPEC'
# Unannotated Feature

## Success Criteria

- [ ] Criterion one
- [ ] Criterion two
- [ ] Criterion three
- [x] Criterion four (checked)

## Out of Scope

Nothing.
SPEC

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/unannotated.md")

  local verify_count judge_count none_count
  verify_count=$(echo "$output" | awk -F'\t' '$2 == "verify"' | wc -l)
  judge_count=$(echo "$output" | awk -F'\t' '$2 == "judge"' | wc -l)
  none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)

  if [ "$verify_count" -eq 0 ] && [ "$judge_count" -eq 0 ]; then
    test_pass "No verify or judge annotations"
  else
    test_fail "Expected 0 verify and 0 judge, got $verify_count verify and $judge_count judge"
  fi

  if [ "$none_count" -eq 4 ]; then
    test_pass "All 4 criteria are unannotated"
  else
    test_fail "Expected 4 unannotated, got $none_count"
  fi

  # Verify checked status is captured
  local checked_count
  checked_count=$(echo "$output" | awk -F'\t' '$5 == "x"' | wc -l)
  if [ "$checked_count" -eq 1 ]; then
    test_pass "One criterion has checked status"
  else
    test_fail "Expected 1 checked criterion, got $checked_count"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: annotation parsing preserves criterion text accurately
#-----------------------------------------------------------------------------
test_spec_criterion_text_preservation() {
  CURRENT_TEST="spec_criterion_text_preservation"
  test_header "Spec Criterion Text Preservation"

  setup_test_env "spec-text"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  cat > "$TEST_DIR/specs/text-test.md" << 'SPEC'
# Text Feature

## Success Criteria

- [ ] `ralph spec` lists all spec files with annotation counts (verify/judge/unannotated)
  [verify](tests/spec-test.sh::test_counts)
- [ ] Criteria with no annotation show as SKIP in verify/judge output
- [ ] `ralph spec --verify` runs shell tests and reports PASS/FAIL/SKIP
  [judge](tests/judges/spec.sh::test_verify)
SPEC

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/text-test.md")

  # Check that criterion text with backticks and special chars is preserved
  local line1
  line1=$(echo "$output" | sed -n '1p')
  if echo "$line1" | grep -q 'ralph spec.*lists all spec files'; then
    test_pass "Preserves backtick-containing criterion text"
  else
    test_fail "Should preserve criterion text with backticks: $line1"
  fi

  local line2
  line2=$(echo "$output" | sed -n '2p')
  if echo "$line2" | grep -q 'no annotation show as SKIP'; then
    test_pass "Preserves criterion text with mixed formatting"
  else
    test_fail "Should preserve criterion text: $line2"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: multiple spec files can be parsed independently
#-----------------------------------------------------------------------------
test_spec_multiple_files() {
  CURRENT_TEST="spec_multiple_files"
  test_header "Parse Multiple Spec Files"

  setup_test_env "spec-multi"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  # Create first spec
  cat > "$TEST_DIR/specs/feature-a.md" << 'SPEC'
# Feature A

## Success Criteria

- [ ] A criterion 1
  [verify](tests/a1.sh::test_a1)
- [ ] A criterion 2

## Design

Design A.
SPEC

  # Create second spec
  cat > "$TEST_DIR/specs/feature-b.md" << 'SPEC'
# Feature B

## Success Criteria

- [ ] B criterion 1
  [judge](tests/judges/b1.sh::test_b1)
- [ ] B criterion 2
  [verify](tests/b2.sh::test_b2)
- [ ] B criterion 3
  [verify](tests/b3.sh)
SPEC

  # Parse each file and verify counts
  local output_a output_b
  output_a=$(parse_spec_annotations "$TEST_DIR/specs/feature-a.md")
  output_b=$(parse_spec_annotations "$TEST_DIR/specs/feature-b.md")

  local count_a count_b
  count_a=$(echo "$output_a" | wc -l)
  count_b=$(echo "$output_b" | wc -l)

  if [ "$count_a" -eq 2 ]; then
    test_pass "Feature A has 2 criteria"
  else
    test_fail "Feature A: expected 2 criteria, got $count_a"
  fi

  if [ "$count_b" -eq 3 ]; then
    test_pass "Feature B has 3 criteria"
  else
    test_fail "Feature B: expected 3 criteria, got $count_b"
  fi

  # Verify annotation types for feature B
  local b_verify b_judge
  b_verify=$(echo "$output_b" | awk -F'\t' '$2 == "verify"' | wc -l)
  b_judge=$(echo "$output_b" | awk -F'\t' '$2 == "judge"' | wc -l)

  if [ "$b_verify" -eq 2 ]; then
    test_pass "Feature B has 2 verify annotations"
  else
    test_fail "Feature B: expected 2 verify, got $b_verify"
  fi

  if [ "$b_judge" -eq 1 ]; then
    test_pass "Feature B has 1 judge annotation"
  else
    test_fail "Feature B: expected 1 judge, got $b_judge"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: annotation parsing handles spec where Success Criteria is at EOF
#-----------------------------------------------------------------------------
test_spec_criteria_at_eof() {
  CURRENT_TEST="spec_criteria_at_eof"
  test_header "Success Criteria at End of File"

  setup_test_env "spec-eof"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  # Spec where Success Criteria is the last section (no closing heading)
  cat > "$TEST_DIR/specs/eof-spec.md" << 'SPEC'
# EOF Feature

## Requirements

Some requirements.

## Success Criteria

- [ ] First criterion
  [verify](tests/first.sh::test_first)
- [ ] Second criterion
  [judge](tests/judges/second.sh::test_second)
- [ ] Third criterion (unannotated at EOF)
SPEC

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/eof-spec.md")
  local line_count
  line_count=$(echo "$output" | wc -l)

  if [ "$line_count" -eq 3 ]; then
    test_pass "Parses all 3 criteria at EOF"
  else
    test_fail "Expected 3 criteria at EOF, got $line_count"
  fi

  # Last criterion should be unannotated
  local last_line
  last_line=$(echo "$output" | tail -1)
  if echo "$last_line" | grep -qP '\tnone\t'; then
    test_pass "Last criterion at EOF is unannotated"
  else
    test_fail "Last criterion at EOF should be unannotated: $last_line"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: fenced code blocks containing ## Success Criteria are skipped
# Regression for wx-6xs5p.19: a spec template embedded in a markdown code
# block would trigger the parser's first match, exiting before the real
# Success Criteria section was reached.
#-----------------------------------------------------------------------------
test_spec_fenced_code_block_ignored() {
  CURRENT_TEST="spec_fenced_code_block_ignored"
  test_header "Spec Parser Ignores Fenced Code Blocks"

  setup_test_env "spec-fenced"

  # shellcheck source=../../lib/ralph/cmd/util.sh
  source "$REPO_ROOT/lib/ralph/cmd/util.sh"

  cat > "$TEST_DIR/specs/fenced.md" << 'SPEC'
# Fenced Feature

## Spec File Format

The template looks like this:

```markdown
# Feature Name

## Success Criteria

- [ ] Template criterion one
- [ ] Template criterion two

## Out of Scope

- Thing not included
```

## Success Criteria

- [ ] Real criterion one
  [verify](tests/real.sh::test_real_one)
- [ ] Real criterion two
  [judge](tests/judges/real.sh::test_real_two)
- [ ] Real criterion three

## Out of Scope

Nothing.
SPEC

  local output
  output=$(parse_spec_annotations "$TEST_DIR/specs/fenced.md")

  local total verify_count judge_count none_count
  total=$(echo "$output" | wc -l)
  verify_count=$(echo "$output" | awk -F'\t' '$2 == "verify"' | wc -l)
  judge_count=$(echo "$output" | awk -F'\t' '$2 == "judge"' | wc -l)
  none_count=$(echo "$output" | awk -F'\t' '$2 == "none"' | wc -l)

  if [ "$total" -eq 3 ]; then
    test_pass "Parses 3 criteria from real section, ignoring template block"
  else
    test_fail "Expected 3 criteria, got $total (template criteria likely not skipped)"
  fi

  if [ "$verify_count" -eq 1 ]; then
    test_pass "Counts 1 verify annotation"
  else
    test_fail "Expected 1 verify, got $verify_count"
  fi

  if [ "$judge_count" -eq 1 ]; then
    test_pass "Counts 1 judge annotation"
  else
    test_fail "Expected 1 judge, got $judge_count"
  fi

  if [ "$none_count" -eq 1 ]; then
    test_pass "Counts 1 unannotated criterion"
  else
    test_fail "Expected 1 unannotated, got $none_count"
  fi

  # Ensure template-only criteria text never leaked into output
  if echo "$output" | grep -q 'Template criterion'; then
    test_fail "Template criteria inside fenced block leaked into parser output"
  else
    test_pass "Template criteria are excluded from parser output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify header includes molecule ID from README.md
#-----------------------------------------------------------------------------
test_spec_verify_molecule_header() {
  CURRENT_TEST="spec_verify_molecule_header"
  test_header "Ralph Spec --verify Molecule ID in Header"

  setup_test_env "spec-verify-mol"

  # Create spec with a verify annotation
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test passes
  [verify](tests/pass-test.sh::test_passes)
SPEC

  # Add README.md entry with molecule ID
  add_readme_spec_entry "test-feature" "wx-abc"

  # Create passing test
  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  local output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  set -e

  # Header should include molecule ID in parentheses
  if echo "$output" | grep -q "Ralph Verify: test-feature (wx-abc)"; then
    test_pass "Header includes molecule ID: Ralph Verify: test-feature (wx-abc)"
  else
    test_fail "Header should include molecule ID. Got: $(echo "$output" | head -1)"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify header works without molecule ID
#-----------------------------------------------------------------------------
test_spec_verify_no_molecule_header() {
  CURRENT_TEST="spec_verify_no_molecule_header"
  test_header "Ralph Spec --verify Header Without Molecule"

  setup_test_env "spec-verify-no-mol"

  # Create spec with a verify annotation
  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test passes
  [verify](tests/pass-test.sh::test_passes)
SPEC

  # README.md has no entry for test-feature (no molecule ID)

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  local output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  set -e

  # Header should show label only, no empty parens
  if echo "$output" | grep -q "Ralph Verify: test-feature$"; then
    test_pass "Header shows label only without molecule ID"
  elif echo "$output" | grep -q "Ralph Verify: test-feature" && ! echo "$output" | grep -q "()"; then
    test_pass "Header shows label only without empty parentheses"
  else
    test_fail "Header should show label only. Got: $(echo "$output" | head -1)"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify --verbose shows captured test output
#-----------------------------------------------------------------------------
test_spec_verify_verbose_output() {
  CURRENT_TEST="spec_verify_verbose_output"
  test_header "Ralph Spec --verify --verbose Shows Output"

  setup_test_env "spec-verify-verbose"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test with output
  [verify](tests/output-test.sh::test_with_output)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/output-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_with_output() {
  echo "diagnostic line 1"
  echo "diagnostic line 2"
  return 0
}
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/output-test.sh"

  # Run --verify --verbose --spec
  local output
  set +e
  output=$(ralph-spec --verify --verbose --spec test-feature 2>&1)
  set -e

  # Should show captured output with pipe prefix
  if echo "$output" | grep -q "| diagnostic line 1"; then
    test_pass "Verbose shows captured output line 1"
  else
    test_fail "Verbose should show captured output. Got: $output"
  fi

  if echo "$output" | grep -q "| diagnostic line 2"; then
    test_pass "Verbose shows captured output line 2"
  else
    test_fail "Verbose should show captured output line 2"
  fi

  # Run --verify without --verbose — should NOT show diagnostic output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  set -e

  if echo "$output" | grep -q "diagnostic line"; then
    test_fail "Non-verbose should not show diagnostic output"
  else
    test_pass "Non-verbose suppresses diagnostic output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: -v short flag maps to --verify (not --verbose)
#-----------------------------------------------------------------------------
test_spec_short_flag_v() {
  CURRENT_TEST="spec_short_flag_v"
  test_header "Ralph Spec -v maps to --verify"

  setup_test_env "spec-short-v"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test passes
  [verify](tests/pass-test.sh::test_passes)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  local output
  set +e
  output=$(ralph-spec -v -s test-feature 2>&1)
  set -e

  # -v should trigger verify mode (shows [PASS]/[FAIL])
  if echo "$output" | grep -q "\[PASS\]"; then
    test_pass "-v triggers verify mode (shows [PASS])"
  else
    test_fail "-v should trigger verify mode. Got: $output"
  fi

  # -v should NOT just be verbose index (which shows [verify]/[judge] annotation types)
  if echo "$output" | grep -q "Ralph Verify"; then
    test_pass "-v produces verify header"
  else
    test_fail "-v should produce verify header. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: -j short flag maps to --judge
#-----------------------------------------------------------------------------
test_spec_short_flag_j() {
  CURRENT_TEST="spec_short_flag_j"
  test_header "Ralph Spec -j maps to --judge"

  setup_test_env "spec-short-j"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Judge criterion
  [judge](tests/judges/quality.sh::test_quality)
SPEC

  mkdir -p "$TEST_DIR/tests/judges"
  cat > "$TEST_DIR/tests/judges/quality.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_quality() {
  judge_files "lib/output.sh"
  judge_criterion "Output is well-formatted"
}
TESTFILE

  local output
  set +e
  output=$(ralph-spec -j -s test-feature 2>&1)
  set -e

  # -j should produce judge header
  if echo "$output" | grep -q "Ralph Judge"; then
    test_pass "-j produces judge header"
  else
    test_fail "-j should produce judge header. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: -a short flag maps to --all
#-----------------------------------------------------------------------------
test_spec_short_flag_a() {
  CURRENT_TEST="spec_short_flag_a"
  test_header "Ralph Spec -a maps to --all"

  setup_test_env "spec-short-a"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Verify criterion
  [verify](tests/check.sh::test_check)
- [ ] No annotation
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/check.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_check() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/check.sh"

  local output
  set +e
  output=$(ralph-spec -a -s test-feature 2>&1)
  set -e

  # -a should produce Verify+Judge header
  if echo "$output" | grep -q "Ralph Verify+Judge"; then
    test_pass "-a produces Verify+Judge header"
  else
    test_fail "-a should produce Verify+Judge header. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: -s short flag maps to --spec
#-----------------------------------------------------------------------------
test_spec_short_flag_s() {
  CURRENT_TEST="spec_short_flag_s"
  test_header "Ralph Spec -s maps to --spec"

  setup_test_env "spec-short-s"

  # Create two specs
  cat > "$TEST_DIR/specs/alpha.md" << 'SPEC'
# Alpha

## Success Criteria

- [ ] Alpha criterion
  [verify](tests/alpha.sh::test_alpha)
SPEC

  cat > "$TEST_DIR/specs/beta.md" << 'SPEC'
# Beta

## Success Criteria

- [ ] Beta criterion
  [verify](tests/beta.sh::test_beta)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/alpha.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_alpha() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/alpha.sh"

  cat > "$TEST_DIR/tests/beta.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_beta() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/beta.sh"

  local output
  set +e
  output=$(ralph-spec -v -s alpha 2>&1)
  set -e

  # -s alpha should only show alpha spec
  if echo "$output" | grep -q "Ralph Verify: alpha"; then
    test_pass "-s filters to single spec (alpha)"
  else
    test_fail "-s should filter to alpha spec. Got: $output"
  fi

  # Should NOT show beta spec
  if echo "$output" | grep -q "beta"; then
    test_fail "-s should not show beta spec"
  else
    test_pass "-s correctly excludes other specs"
  fi

  # Single-spec format should NOT have "Summary:" prefix
  if echo "$output" | grep -q "^Summary:"; then
    test_fail "Single-spec mode should not have Summary: prefix"
  else
    test_pass "Single-spec mode has no Summary: prefix"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: --verbose has no short -v flag (it was reassigned to --verify)
#-----------------------------------------------------------------------------
test_spec_verbose_no_short_v() {
  CURRENT_TEST="spec_verbose_no_short_v"
  test_header "Ralph Spec --verbose has no -v short flag"

  setup_test_env "spec-verbose-no-v"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Some criterion
  [verify](tests/check.sh::test_check)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/check.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_check() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/check.sh"

  # -v should trigger verify (not verbose)
  local output
  set +e
  output=$(ralph-spec -v -s test-feature 2>&1)
  set -e

  # If -v were verbose, it would show annotation index with [verify]/[judge] types
  # Since -v is now verify, it should show [PASS]/[FAIL]/[SKIP]
  if echo "$output" | grep -q "\[PASS\]"; then
    test_pass "-v triggers verify, not verbose"
  else
    test_fail "-v should trigger verify. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: short flags compose: -vj is equivalent to --all
#-----------------------------------------------------------------------------
test_spec_short_compose() {
  CURRENT_TEST="spec_short_compose"
  test_header "Ralph Spec -vj composes to --all"

  setup_test_env "spec-short-compose"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Verify criterion
  [verify](tests/check.sh::test_check)
- [ ] No annotation
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/check.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_check() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/check.sh"

  local output
  set +e
  output=$(ralph-spec -vj -s test-feature 2>&1)
  set -e

  # -vj should produce Verify+Judge header (equivalent to --all)
  if echo "$output" | grep -q "Ralph Verify+Judge"; then
    test_pass "-vj composes to Verify+Judge"
  else
    test_fail "-vj should compose to Verify+Judge. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: multi-spec verify groups results by spec with per-spec headers
#-----------------------------------------------------------------------------
test_spec_multi_grouped_output() {
  CURRENT_TEST="spec_multi_grouped_output"
  test_header "Multi-spec Grouped Output"

  setup_test_env "spec-multi-grouped"

  # Create two specs with verify annotations
  cat > "$TEST_DIR/specs/alpha.md" << 'SPEC'
# Alpha

## Success Criteria

- [ ] Alpha passes
  [verify](tests/alpha.sh::test_alpha)
SPEC

  cat > "$TEST_DIR/specs/beta.md" << 'SPEC'
# Beta

## Success Criteria

- [ ] Beta passes
  [verify](tests/beta.sh::test_beta)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/alpha.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_alpha() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/alpha.sh"

  cat > "$TEST_DIR/tests/beta.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_beta() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/beta.sh"

  local output
  set +e
  output=$(ralph-spec --verify 2>&1)
  set -e

  # Should have per-spec headers
  if echo "$output" | grep -q "Ralph Verify: alpha"; then
    test_pass "Shows alpha spec header"
  else
    test_fail "Should show alpha spec header. Got: $output"
  fi

  if echo "$output" | grep -q "Ralph Verify: beta"; then
    test_pass "Shows beta spec header"
  else
    test_fail "Should show beta spec header. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: multi-spec output ends with summary line including spec count
#-----------------------------------------------------------------------------
test_spec_multi_summary_line() {
  CURRENT_TEST="spec_multi_summary_line"
  test_header "Multi-spec Summary Line"

  setup_test_env "spec-multi-summary"

  cat > "$TEST_DIR/specs/alpha.md" << 'SPEC'
# Alpha

## Success Criteria

- [ ] Alpha passes
  [verify](tests/alpha.sh::test_alpha)
SPEC

  cat > "$TEST_DIR/specs/beta.md" << 'SPEC'
# Beta

## Success Criteria

- [ ] Beta passes
  [verify](tests/beta.sh::test_beta)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/alpha.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_alpha() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/alpha.sh"

  cat > "$TEST_DIR/tests/beta.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_beta() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/beta.sh"

  local output
  set +e
  output=$(ralph-spec --verify 2>&1)
  set -e

  # Should end with "Summary: X passed, Y failed, Z skipped (N specs)"
  if echo "$output" | grep -q "^Summary:.*passed.*failed.*skipped.*(2 specs)"; then
    test_pass "Summary line has correct format with spec count"
  else
    test_fail "Summary line should match 'Summary: X passed, Y failed, Z skipped (N specs)'. Got: $(echo "$output" | tail -1)"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: exit code is non-zero if any spec has a failure in multi-spec mode
#-----------------------------------------------------------------------------
test_spec_nonzero_exit() {
  CURRENT_TEST="spec_nonzero_exit"
  test_header "Multi-spec Non-zero Exit on Failure"

  setup_test_env "spec-nonzero-exit"

  # Create one passing spec and one failing spec
  cat > "$TEST_DIR/specs/pass-spec.md" << 'SPEC'
# Passing Spec

## Success Criteria

- [ ] This passes
  [verify](tests/pass.sh::test_pass)
SPEC

  cat > "$TEST_DIR/specs/fail-spec.md" << 'SPEC'
# Failing Spec

## Success Criteria

- [ ] This fails
  [verify](tests/fail.sh::test_fail)
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/pass.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_pass() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass.sh"

  cat > "$TEST_DIR/tests/fail.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_fail() { return 1; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/fail.sh"

  set +e
  ralph-spec --verify >/dev/null 2>&1
  local exit_code=$?
  set -e

  if [ "$exit_code" -ne 0 ]; then
    test_pass "Exit code is non-zero when any spec fails ($exit_code)"
  else
    test_fail "Exit code should be non-zero when a spec fails"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: specs with no success criteria are silently skipped in multi-spec mode
#-----------------------------------------------------------------------------
test_spec_skip_empty() {
  CURRENT_TEST="spec_skip_empty"
  test_header "Multi-spec Skips Specs Without Success Criteria"

  setup_test_env "spec-skip-empty"

  # Create a spec with criteria
  cat > "$TEST_DIR/specs/has-criteria.md" << 'SPEC'
# Has Criteria

## Success Criteria

- [ ] This passes
  [verify](tests/pass.sh::test_pass)
SPEC

  # Create a spec WITHOUT criteria
  cat > "$TEST_DIR/specs/no-criteria.md" << 'SPEC'
# No Criteria

## Requirements

Some requirements but no success criteria.
SPEC

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/pass.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_pass() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass.sh"

  local output
  set +e
  output=$(ralph-spec --verify 2>&1)
  set -e

  # Should show the spec with criteria
  if echo "$output" | grep -q "Ralph Verify: has-criteria"; then
    test_pass "Shows spec with criteria"
  else
    test_fail "Should show spec with criteria. Got: $output"
  fi

  # Should NOT show the spec without criteria
  if echo "$output" | grep -q "no-criteria"; then
    test_fail "Should not show spec without criteria"
  else
    test_pass "Silently skips spec without criteria"
  fi

  # Summary should count only 1 spec
  if echo "$output" | grep -q "(1 specs)"; then
    test_pass "Summary counts only specs with criteria"
  else
    test_fail "Summary should count 1 spec. Got: $(echo "$output" | tail -1)"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: multi-spec verify with molecule IDs from README.md
#-----------------------------------------------------------------------------
test_spec_multi_molecule_lookup() {
  CURRENT_TEST="spec_multi_molecule_lookup"
  test_header "Multi-spec Molecule ID Lookup from README.md"

  setup_test_env "spec-multi-mol"

  # Create specs
  cat > "$TEST_DIR/specs/alpha.md" << 'SPEC'
# Alpha

## Success Criteria

- [ ] Alpha passes
  [verify](tests/alpha.sh::test_alpha)
SPEC

  cat > "$TEST_DIR/specs/beta.md" << 'SPEC'
# Beta

## Success Criteria

- [ ] Beta passes
  [verify](tests/beta.sh::test_beta)
SPEC

  # Add README.md entries with molecule IDs
  add_readme_spec_entry "alpha" "wx-aaa"
  add_readme_spec_entry "beta" "wx-bbb"

  mkdir -p "$TEST_DIR/tests"
  cat > "$TEST_DIR/tests/alpha.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_alpha() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/alpha.sh"

  cat > "$TEST_DIR/tests/beta.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_beta() { return 0; }
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/beta.sh"

  local output
  set +e
  output=$(ralph-spec --verify 2>&1)
  set -e

  # Headers should include molecule IDs from README.md
  if echo "$output" | grep -q "Ralph Verify: alpha (wx-aaa)"; then
    test_pass "Alpha header includes molecule ID wx-aaa"
  else
    test_fail "Alpha header should include wx-aaa. Got: $output"
  fi

  if echo "$output" | grep -q "Ralph Verify: beta (wx-bbb)"; then
    test_pass "Beta header includes molecule ID wx-bbb"
  else
    test_fail "Beta header should include wx-bbb. Got: $output"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Test: ralph spec --verify shows failure output without --verbose (FR5)
#-----------------------------------------------------------------------------
test_spec_verify_fail_shows_output() {
  CURRENT_TEST="spec_verify_fail_shows_output"
  test_header "Ralph Spec --verify Shows Failure Output Without --verbose"

  setup_test_env "spec-verify-fail-output"

  cat > "$TEST_DIR/specs/test-feature.md" << 'SPEC'
# Test Feature

## Success Criteria

- [ ] Test with failure output
  [verify](tests/fail-test.sh::test_fails_with_output)
- [ ] Test that passes with output
  [verify](tests/pass-test.sh::test_passes_with_output)
SPEC

  mkdir -p "$TEST_DIR/tests"

  cat > "$TEST_DIR/tests/fail-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_fails_with_output() {
  echo "setting up environment"
  echo "loading config"
  echo "ERROR: foo-binary: command not found"
  return 1
}
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/fail-test.sh"

  cat > "$TEST_DIR/tests/pass-test.sh" << 'TESTFILE'
#!/usr/bin/env bash
test_passes_with_output() {
  echo "diagnostic output from passing test"
  return 0
}
if [ $# -gt 0 ]; then "$@"; fi
TESTFILE
  chmod +x "$TEST_DIR/tests/pass-test.sh"

  # Run --verify WITHOUT --verbose
  local output
  set +e
  output=$(ralph-spec --verify --spec test-feature 2>&1)
  set -e

  # On FAIL: should show tail of output even without --verbose
  if echo "$output" | grep -q "| .*foo-binary: command not found"; then
    test_pass "Failure output shown without --verbose"
  else
    test_fail "Should show failure output without --verbose. Got: $output"
  fi

  # On PASS: should NOT show output without --verbose
  if echo "$output" | grep -q "diagnostic output from passing test"; then
    test_fail "Pass output should not be shown without --verbose"
  else
    test_pass "Pass output correctly suppressed without --verbose"
  fi

  teardown_test_env
}

#-----------------------------------------------------------------------------
# Main Test Runner
#-----------------------------------------------------------------------------

ALL_TESTS=(
  test_spec_annotation_counts
  test_spec_verbose
  test_spec_verify
  test_spec_verify_skip_exits
  test_spec_judge
  test_spec_all
  test_spec_no_execution_default
  test_spec_annotation_counting
  test_spec_verify_only
  test_spec_all_unannotated
  test_spec_criterion_text_preservation
  test_spec_multiple_files
  test_spec_criteria_at_eof
  test_spec_fenced_code_block_ignored
  test_spec_verify_molecule_header
  test_spec_verify_no_molecule_header
  test_spec_verify_verbose_output
  test_spec_short_flag_v
  test_spec_short_flag_j
  test_spec_short_flag_a
  test_spec_short_flag_s
  test_spec_verbose_no_short_v
  test_spec_short_compose
  test_spec_multi_grouped_output
  test_spec_multi_summary_line
  test_spec_nonzero_exit
  test_spec_skip_empty
  test_spec_multi_molecule_lookup
  test_spec_verify_fail_shows_output
)

main() {
  echo "=========================================="
  echo "  Ralph Spec Integration Tests"
  echo "=========================================="
  echo ""
  echo "Test directory: $SCRIPT_DIR"
  echo "Repo root: $REPO_ROOT"
  echo ""

  # Check prerequisites
  check_prerequisites "$MOCK_CLAUDE" "$SCENARIOS_DIR" || exit 1

  run_tests ALL_TESTS "${1:-}"
}

main "$@"
