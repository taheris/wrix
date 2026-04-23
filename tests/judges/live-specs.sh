#!/usr/bin/env bash
# Judge rubrics for live-specs.md success criteria

test_spec_index_output() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec lists all spec files with per-file annotation counts showing verify, judge, and unannotated totals"
}

test_spec_verbose_output() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --verbose expands output to show per-criterion detail with each criterion and its annotation type"
}

test_spec_verify_runner() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --verify runs shell tests referenced by [verify] annotations and reports PASS/FAIL/SKIP per criterion"
}

test_spec_judge_runner() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --judge invokes LLM with rubric from [judge] annotations and reports PASS/FAIL/SKIP per criterion"
}

test_spec_all_flag() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --all runs both verify and judge checks (equivalent to --verify --judge)"
}

test_spec_instant_default() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec with no flags is instant — it only reads and parses spec files, never executing tests or making LLM calls"
}

test_status_watch_tmux() {
  judge_files "lib/ralph/cmd/status.sh"
  judge_criterion "ralph status --watch creates a tmux split with auto-refreshing status in top pane and agent output in bottom pane"
}

test_status_watch_no_tmux() {
  judge_files "lib/ralph/cmd/status.sh"
  judge_criterion "ralph status --watch errors with a clear message when not running inside a tmux session"
}

test_status_watch_standalone() {
  judge_files "lib/ralph/cmd/status.sh"
  judge_criterion "ralph status --watch works standalone showing status and recent activity even when no ralph run session is active"
}

test_run_skips_awaiting() {
  judge_files "lib/ralph/cmd/run.sh"
  judge_criterion "ralph run skips beads with the ralph:clarify label, treating them as not ready for automated processing"
}

test_status_awaiting_display() {
  judge_files "lib/ralph/cmd/status.sh"
  judge_criterion "ralph status displays awaiting items distinctly, showing the question text and how long ago it was asked"
}

test_judge_rubric_format() {
  judge_files "tests/judges/notifications.sh" "tests/judges/ralph-workflow.sh"
  judge_criterion "Judge test files define rubrics using judge_files to specify source files and judge_criterion to specify evaluation criteria"
}

test_clickable_links() {
  judge_files "specs/notifications.md" "specs/ralph-harness.md"
  judge_criterion "Annotations use standard markdown link syntax [verify](path) and [judge](path) that renders as clickable links in GitHub and VS Code"
}

test_spec_verify_all_specs() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --verify runs across all spec files"
}

test_spec_judge_all_specs() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --judge runs across all spec files"
}

test_spec_all_all_specs() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --all runs both across all spec files"
}

test_spec_filter_single() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "ralph spec --verify --spec notifications filters to single spec"
}

test_spec_grouped_output() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "Multi-spec output groups results by spec with per-spec headers"
}

test_spec_summary_line() {
  judge_files "lib/ralph/cmd/spec.sh"
  judge_criterion "Multi-spec output ends with summary line"
}
