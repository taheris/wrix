#!/usr/bin/env bash
# Judge rubrics for ralph-workflow.md success criteria

test_plan_update_writes_new_requirements() {
  judge_files "lib/ralph/cmd/plan.sh"
  judge_criterion "ralph plan -u writes NEW requirements to state/<label>.md rather than modifying the main spec"
}

test_plan_update_hidden() {
  judge_files "lib/ralph/cmd/plan.sh"
  judge_criterion "ralph plan -u -h updates a hidden spec in state/ directory"
}

test_plan_runs_in_container() {
  judge_files "lib/ralph/cmd/plan.sh"
  judge_criterion "ralph plan runs Claude in a wrapix container using the base profile"
}

test_todo_update_reads_new_requirements() {
  judge_files "lib/ralph/cmd/todo.sh"
  judge_criterion "In update mode, ralph todo reads NEW requirements from state/<label>.md"
}

test_todo_update_creates_only_new() {
  judge_files "lib/ralph/cmd/todo.sh"
  judge_criterion "In update mode, ralph todo creates tasks ONLY for new requirements, not duplicating existing ones"
}

test_todo_update_merges_state() {
  judge_files "lib/ralph/cmd/todo.sh"
  judge_criterion "In update mode, ralph todo merges state/<label>.md into specs/<label>.md after creating tasks"
}

test_todo_update_deletes_state() {
  judge_files "lib/ralph/cmd/todo.sh"
  judge_criterion "In update mode, ralph todo deletes state/<label>.md after successful merge into the main spec"
}

test_todo_runs_in_container() {
  judge_files "lib/ralph/cmd/todo.sh"
  judge_criterion "ralph todo runs Claude in a wrapix container using the base profile"
}

test_todo_update_with_diff() {
  judge_files "lib/ralph/template/todo-update.md" "lib/ralph/template/default.nix"
  judge_criterion "todo-update.md template uses SPEC_DIFF variable to show git diff output, and instructs the LLM to create tasks only for added/changed lines in the diff"
}

test_todo_update_with_tasks() {
  judge_files "lib/ralph/template/todo-update.md" "lib/ralph/template/default.nix"
  judge_criterion "todo-update.md template uses EXISTING_TASKS variable to show current molecule tasks, and instructs the LLM to compare against the spec to identify gaps when SPEC_DIFF is empty"
}

test_run_already_implemented() {
  judge_files "lib/ralph/template/run.md"
  judge_criterion "run.md template includes guidance for already-implemented tasks: verify correctness, close the issue, and output RALPH_COMPLETE"
}

test_todo_new_writes_readme_beads() {
  judge_files "lib/ralph/template/todo-new.md" "lib/ralph/cmd/todo.sh"
  judge_criterion "todo-new.md template instructs the LLM to write the molecule ID to the specs/README.md Beads column, and the instruction emphasizes this is required for cross-machine state recovery"
}

test_todo_update_fills_readme_beads() {
  judge_files "lib/ralph/template/todo-update.md"
  judge_criterion "todo-update.md template instructs the LLM to check if specs/README.md Beads column is empty for this spec, and if so, fill in the molecule ID"
}

test_plan_update_invariant_clash_detection() {
  judge_files "lib/ralph/template/plan-update.md"
  judge_criterion "plan-update.md instructs the LLM to scan the existing spec for invariants (architectural decisions, data-structure choices, explicit constraints, non-functional requirements, out-of-scope items) before committing a change, pause the interview when a potential clash is found, and propose contextual options (typically 2-4) guided by the three-paths principle (preserve invariant / keep on top inelegantly / change invariant) but not limited to a fixed A/B/C menu. The template biases toward asking when uncertain."
}

test_tune_interactive() {
  judge_files "lib/ralph/cmd/tune.sh"
  judge_criterion "ralph tune in interactive mode identifies the correct template to edit and allows making changes"
}

test_tune_integration() {
  judge_files "lib/ralph/cmd/tune.sh"
  judge_criterion "ralph tune in integration mode ingests a diff and interviews the user about changes"
}
