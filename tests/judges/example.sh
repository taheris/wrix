#!/usr/bin/env bash
# Example judge rubric — reference for writing new judge tests.
#
# Each function defines a rubric by calling:
#   judge_files   — source files the LLM reads
#   judge_criterion — what the LLM evaluates
#
# Rubric functions are referenced from spec success criteria annotations
# using a spec-relative path and a markdown `#fn` fragment selector so
# the link is clickable in markdown renderers:
#   - [ ] Output includes progress percentage
#     [judge](../tests/judges/example.sh#test_progress_display)
#
# When ralph spec --judge runs, it sources this file, calls the function,
# then passes the files + criterion to an LLM for PASS/FAIL evaluation.

test_progress_display() {
  judge_files "lib/ralph/cmd/status.sh"
  judge_criterion "Output includes progress percentage and status indicators for each issue"
}

test_clear_error_messages() {
  judge_files "lib/ralph/cmd/util.sh"
  judge_criterion "Error messages are clear and actionable, telling the user what went wrong and how to fix it"
}
