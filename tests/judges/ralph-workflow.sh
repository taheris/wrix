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
  judge_criterion "todo-new.md template instructs the LLM to write the molecule ID to the spec index Beads column (the pinned-context file, docs/README.md by default), and the instruction emphasizes this is required for cross-machine state recovery"
}

test_todo_update_fills_readme_beads() {
  judge_files "lib/ralph/template/todo-update.md"
  judge_criterion "todo-update.md template instructs the LLM to check if the spec index (the pinned-context file, docs/README.md by default) has an empty Beads column for this spec, and if so, fill in the molecule ID"
}

test_plan_update_invariant_clash_detection() {
  judge_files "lib/ralph/template/plan-update.md"
  judge_criterion "plan-update.md instructs the LLM to scan the existing spec for invariants (architectural decisions, data-structure choices, explicit constraints, non-functional requirements, out-of-scope items) before committing a change, pause the interview when a potential clash is found, and propose contextual options (typically 2-4) guided by the three-paths principle (preserve invariant / keep on top inelegantly / change invariant) but not limited to a fixed A/B/C menu. The template biases toward asking when uncertain."
}

test_plan_anchor_sibling_editing() {
  judge_files "lib/ralph/template/plan-update.md"
  judge_criterion "plan-update.md explains that the label named on '-u' is the anchor and owns state/<label>.json (holding molecule, implementation_notes, iteration_count), that the LLM may read and edit any spec in specs/ when a change cross-cuts (sibling-spec editing, no pre-declaration, touched set emerges from the interview), that docs/README.md is the spec index to consult for locating siblings, and that hidden specs (-u -h) are single-spec and do NOT participate in sibling editing. The template instructs the LLM to commit the anchor and every touched sibling spec at end of session (git-tracked specs only; hidden specs just save the file)."
}

test_plan_cross_spec_invariant_clash() {
  judge_files "lib/ralph/template/plan-update.md"
  judge_criterion "plan-update.md's invariant-clash scan explicitly covers the anchor spec AND any touched sibling specs — not just specs/<LABEL>.md — so a change landing in the anchor that contradicts an invariant in a sibling is caught before commit. The three-paths principle remains guidance (not a fixed menu), and the template still biases toward asking when uncertain."
}

test_tune_interactive() {
  judge_files "lib/ralph/cmd/tune.sh"
  judge_criterion "ralph tune in interactive mode identifies the correct template to edit and allows making changes"
}

test_tune_integration() {
  judge_files "lib/ralph/cmd/tune.sh"
  judge_criterion "ralph tune in integration mode ingests a diff and interviews the user about changes"
}

test_repin_after_compaction() {
  judge_files "lib/ralph/cmd/run.sh" "lib/ralph/cmd/check.sh" "lib/ralph/cmd/plan.sh" "lib/ralph/cmd/todo.sh" "lib/sandbox/linux/entrypoint.sh"
  judge_criterion "The ralph commands write a repin.sh and a claude-settings.json fragment to .wrapix/ralph/runtime/<label>/ before launching the container, the settings fragment registers a SessionStart hook with matcher \"compact\" pointing at repin.sh that emits hookSpecificOutput.additionalContext JSON containing the condensed re-pin (spec header, beads summary, current bead context — NOT the full spec body), and the sandbox entrypoint merges \$RALPH_RUNTIME_DIR/claude-settings.json into ~/.claude/settings.json so that after the model auto-compacts the session, the very next model turn receives the re-pin via the additionalContext channel rather than starting from a context-stripped state. Hooks are concatenated (not replaced) so coexisting sandbox hooks survive."
}

test_check_contextual_options() {
  judge_files "lib/ralph/template/check.md"
  judge_criterion "check.md instructs the reviewer, when an invariant clash is detected, to propose options that are CONTEXTUAL to that specific clash (typically 2–4 options, each naming its concrete cost such as churn, debt, coupling, or risk) rather than emitting a generic fixed A/B/C menu. The three-paths principle (preserve / keep on top / change the invariant) is presented as guidance and lens, not as a rigid template, and the template explicitly forbids a one-size-fits-all menu."
}

test_check_clarify_bead_shape() {
  judge_files "lib/ralph/template/check.md"
  judge_criterion "check.md instructs the reviewer, for every detected invariant clash, to create a bead via 'bd create' that (1) carries the 'ralph:clarify' label, (2) is parented to the molecule (and bonded with 'bd mol bond'), and (3) has a description containing the proposed contextual options for that specific clash so the user can pick a path with 'ralph msg'. The shape includes a clear summary of the clash, evidence, and the per-option cost so the user has enough to decide without re-deriving context."
}

test_plan_one_by_one_mode() {
  judge_files "lib/ralph/template/partial/interview-modes.md" "lib/ralph/template/plan-new.md" "lib/ralph/template/plan-update.md"
  judge_criterion "The interview-modes partial (wired into plan-new.md and plan-update.md) instructs the LLM to recognize 'one by one' and close variants (e.g. 'let's go through one by one', 'go through them one at a time') via loose phrase matching on intent rather than exact wording, and to respond by walking the open design questions one at a time, each accompanied by a suggested default + short rationale, and waiting for the user to accept/reject/adjust before moving to the next. The mode optimizes for low per-turn cognitive load with rubber-stampable defaults."
}

test_plan_polish_mode() {
  judge_files "lib/ralph/template/partial/interview-modes.md" "lib/ralph/template/plan-new.md" "lib/ralph/template/plan-update.md"
  judge_criterion "The interview-modes partial (wired into plan-new.md and plan-update.md) instructs the LLM to recognize 'polish the spec' and close variants (e.g. 'polish this spec', 'give it a polish', 'do a polish pass') via loose phrase matching, and to respond by reading the full spec end-to-end and reporting findings across readability, consistency, ambiguity, and structural dimensions, each finding accompanied by a SPECIFIC proposed edit (not vague suggestions). Both interview modes remain planning-only — no code changes are produced."
}

test_init_output_format() {
  judge_files "lib/ralph/cmd/init.sh"
  judge_criterion "ralph init prints a per-artifact summary on exit: a leading '✓ Bootstrapped wrapix project in .' banner, a 'Created:' section listing each artifact the run produced (including flake.nix, .envrc, .gitignore with '(N entries appended)' detail, .pre-commit-config.yaml, docs/README.md, docs/architecture.md, docs/style-guidelines.md, AGENTS.md, CLAUDE.md with '(-> AGENTS.md)' detail, and .wrapix/ralph/template/), a 'Skipped:' section listing each artifact that was left alone with a parenthetical reason (e.g. '.beads/  (already initialized)'), and a fixed two-step 'Next steps:' block — 'direnv allow' then 'ralph plan -n <label>' — followed by a 'Docs: specs/ralph-harness.md' pointer. The next-steps block is fixed (always the same two items + docs pointer) regardless of what was created or skipped."
}

test_todo_fanout_worked_example() {
  judge_files "lib/ralph/cmd/todo.sh" "lib/ralph/cmd/util.sh" "lib/ralph/template/todo-update.md"
  judge_criterion "Given the worked example in specs/ralph-loop.md (anchor 'auth' with base_commit X; sibling 'auth-ui' with its own base_commit Y > X; new sibling 'auth-admin' with no state file; commits C1, C2, C3 landed on auth.md, auth-ui.md, auth-admin.md respectively during the plan session), the ralph todo implementation (command + util helpers + todo-update.md template) produces the correct outcome: (1) the tier 1 candidate set is derived from 'git diff X..HEAD --name-only -- specs/' and contains all three spec files; (2) per-spec diffs use each spec's own effective base — auth.md uses X..HEAD, auth-ui.md uses Y..HEAD (NOT X..HEAD, to avoid re-creating tasks already created last week), and auth-admin.md uses X..HEAD (seeded from anchor since no state file exists); (3) every new task — regardless of which spec it implements — bonds to the anchor's single molecule with its --parent, and carries a 'spec:<label>' label naming the spec it implements so anchor- vs sibling-spec tasks are attributable; (4) on RALPH_COMPLETE, every spec in the candidate set that received at least one task has its state/<label>.base_commit advanced to HEAD, the missing state/auth-admin.json is created on demand with {label, spec_path, base_commit, companions: []}, and the anchor's implementation_notes is cleared atomically with its base_commit advancement; (5) specs in the candidate set that did NOT receive tasks do NOT have their cursor advanced."
}

test_check_options_format_conformance() {
  judge_files "lib/ralph/template/check.md"
  judge_criterion "check.md mandates the Options Format Contract for every reviewer-created invariant-clash clarify bead: the bead description MUST contain a '## Options — <summary>' header (em-dash, en-dash, single or double hyphen accepted; em-dash is the default) and at least one '### Option N — <title>' subsection where N is 1-based sequential, each option body naming its concrete cost (churn, debt, coupling, risk). The template worked example in its 'bd create ... --description=' block demonstrates the shape end-to-end (a '## Options — <summary>' line followed by '### Option 1 — ...', '### Option 2 — ...', '### Option 3 — ...' subsections). The template explicitly frames this as REQUIRED (not optional) because ralph msg parses the format for SUMMARY rendering, view-mode enumeration, and integer fast-reply — a malformed bead silently breaks '-a <int>' lookup."
}

test_msg_interactive_triage() {
  judge_files "lib/ralph/template/msg.md"
  judge_criterion "msg.md instructs the Drafter session to present a triage summary of all outstanding clarify beads BEFORE walking through them: print one line per bead framed by each bead's '## Options — <summary>' header (falling back to the bead title when the header is absent), numbered so the user can refer to beads by index, and only then ask the user to pick an order (or accept the printed order) before entering the per-bead walk. The triage-summary step precedes per-bead resolution in the 'Session Flow' of the template and is not skipped even when there is only one bead."
}

test_msg_interactive_clears_label() {
  judge_files "lib/ralph/template/msg.md"
  judge_criterion "msg.md instructs the Drafter, for each clarify bead, to BOTH (1) write a self-contained resolution note via 'bd update <id> --notes' AND (2) remove the clarify label via 'bd update <id> --remove-label=ralph:clarify' — both actions happen per bead, after user confirmation of the drafted note. The note is explicitly self-contained (states what was decided, why, and consequences) so that a reader a month later understands the decision without re-reading the bead description, which may have been edited. The template shows both bd commands in the per-bead step (not just one), and clearing the label is not conditional on anything beyond the user confirming the note."
}

test_rule_partial_discipline() {
  judge_files \
    "lib/ralph/template/plan-new.md" \
    "lib/ralph/template/plan-update.md" \
    "lib/ralph/template/todo-new.md" \
    "lib/ralph/template/todo-update.md" \
    "lib/ralph/template/run.md" \
    "lib/ralph/template/check.md" \
    "lib/ralph/template/msg.md"
  judge_criterion "Rule-shaped prose — conventions, protocols, 'MUST'/'SHOULD'/'NEVER' statements, mandatory process steps, or non-derivable discipline the model cannot reconstruct from reading the spec, codebase, or 'bd show' — lives ONLY inside '{{> partial}}' references within these template bodies. Inline content outside {{> }} references is limited to orientation (label, spec path, issue metadata, companion paths, exit-signal placeholders), role/first-turn framing (e.g. 'You are conducting a specification interview'), or data-variable placeholders ({{SPEC_CONTENT}}, {{EXISTING_SPEC}}, {{DESCRIPTION}}, etc.). A passing judgement reports no rule-shaped prose appearing inline outside a {{> partial}} reference; a failing judgement names each offending inline rule with file + approximate location and the partial it should have been extracted to. This invariant is the structural contract that makes Compaction Re-Pin faithful: partials are re-injected after auto-compact, inline rule prose is not."
}

test_repin_restores_template_rules_after_compact() {
  judge_files \
    "lib/ralph/cmd/util.sh" \
    "lib/ralph/cmd/plan.sh" \
    "lib/ralph/template/plan-new.md" \
    "lib/ralph/template/plan-update.md" \
    "lib/ralph/template/partial/implementation-notes-spec.md" \
    "lib/ralph/template/partial/implementation-notes-state.md" \
    "lib/ralph/template/partial/sibling-spec-editing.md" \
    "lib/ralph/template/partial/invariant-clash.md" \
    "specs/ralph-harness.md"
  judge_criterion "After auto-compaction in a long-running 'ralph plan -u <label>' session, the SessionStart[compact] re-pin — produced by build_repin_content and delivered via repin.sh — restores template-defined conventions that the model would otherwise lose. Evaluate end-to-end that the fix for wx-p9tzi holds: (1) build_repin_content scans the running command's template (plan-update.md for '-u' sessions) for {{> partial}} references and appends each resolved partial body after the orientation block; (2) for plan-new.md the re-pin restores the '## Implementation Notes' spec-markdown convention (via partial/implementation-notes-spec.md); (3) for plan-update.md the re-pin restores the 'implementation_notes lives in state JSON, not spec markdown' convention (via partial/implementation-notes-state.md), the sibling-spec editing protocol (partial/sibling-spec-editing.md), and the invariant-clash three-paths principle (partial/invariant-clash.md); (4) these partials carry the rule prose verbatim so the model retains the conventions WITHOUT needing to re-read specs/<label>.md or this spec; (5) the orientation block does NOT re-inject the full spec body (that's data, re-readable on demand). A passing judgement confirms the re-pin pipeline delivers each named convention to the post-compact session. A failing judgement names which convention is missing or where the pipeline breaks."
}
