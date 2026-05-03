#!/usr/bin/env bash
# Judge rubrics for loom-harness.md success criteria.
#
# Each function describes a rubric the judge LLM evaluates against the
# referenced source files; the spec links to the function via a
# `[judge](tests/judges/loom.sh::<name>)` annotation in its Success Criteria.

test_git_client_encapsulation() {
  judge_files \
    "loom/crates/loom-core/src/git/mod.rs" \
    "loom/crates/loom-core/src/git/client.rs" \
    "loom/crates/loom-core/src/git/error.rs" \
    "loom/crates/loom-core/src/lib.rs" \
    "loom/crates/loom/src/main.rs" \
    "loom/crates/loom-agent/src/lib.rs" \
    "loom/crates/loom-workflow/src/lib.rs" \
    "loom/crates/loom-templates/src/lib.rs"
  judge_criterion \
    "GitClient (loom/crates/loom-core/src/git/) is the ONLY module that imports the gix crate or invokes the git CLI (Command::new(\"git\") or shell-out). Outside the git module, no source file may 'use gix' or spawn git directly. Callers see only typed Rust methods (status, diff_head_parent, worktrees, create_worktree, remove_worktree, merge_branch). Verify by inspecting every listed file: only files under loom-core/src/git/ may reference gix or invoke git; the other crates and lib.rs / main.rs must not."
}

test_template_context_structs() {
  judge_files \
    "loom/crates/loom-templates/src/lib.rs" \
    "loom/crates/loom-templates/src/plan/mod.rs" \
    "loom/crates/loom-templates/src/plan/new.rs" \
    "loom/crates/loom-templates/src/plan/update.rs" \
    "loom/crates/loom-templates/src/todo/mod.rs" \
    "loom/crates/loom-templates/src/todo/new.rs" \
    "loom/crates/loom-templates/src/todo/update.rs" \
    "loom/crates/loom-templates/src/run/mod.rs" \
    "loom/crates/loom-templates/src/check/mod.rs" \
    "loom/crates/loom-templates/src/msg/mod.rs"
  judge_criterion \
    "Each Loom workflow template has a typed Rust context struct with #[derive(askama::Template)] and the correct #[template(path = ...)] attribute. Module structure is nested per template family — no central types.rs at the crate root, no shared error.rs; lib.rs only declares pub mod for plan, todo, run, check, msg. Domain identifier fields use the loom-core newtypes (BeadId, MoleculeId, SpecLabel) — never bare String — for issue_id, molecule_id, label. Optional fields use Option<T> (spec_diff, existing_tasks, molecule_id, issue_id, title, description, beads_summary, base_commit, previous_failure). Multi-valued fields use Vec<T> (companion_paths: Vec<String>, implementation_notes: Vec<String>, clarify_beads: Vec<ClarifyBead>). PreviousFailure is its own type that enforces the 4000-char truncation cap from the spec — RunContext stores Option<PreviousFailure>, not Option<String>. ClarifyBead and ClarifyOption live alongside MsgContext in msg/mod.rs (the only template that uses them). Templates declare escape = \"none\" so markdown bodies are not HTML-escaped."
}

test_newtypes_for_identifiers() {
  judge_files \
    "loom/crates/loom-core/src/identifier/mod.rs" \
    "loom/crates/loom-core/src/identifier/bead.rs" \
    "loom/crates/loom-core/src/identifier/spec.rs" \
    "loom/crates/loom-core/src/identifier/molecule.rs" \
    "loom/crates/loom-core/src/identifier/profile.rs" \
    "loom/crates/loom-core/src/identifier/session.rs" \
    "loom/crates/loom-core/src/identifier/tool_call.rs" \
    "loom/crates/loom-core/src/identifier/request.rs" \
    "loom/crates/loom-core/src/agent/mod.rs"
  judge_criterion \
    "Domain and protocol identifiers are newtype wrappers, not bare strings. The newtype_id! macro in identifier/mod.rs produces a #[serde(transparent)] tuple struct around String with new(impl Into<String>) -> Self, as_str(&self) -> &str, and a Display impl that writes the inner string. Each id family lives in its own submodule under identifier/ — bead.rs (BeadId), spec.rs (SpecLabel), molecule.rs (MoleculeId), profile.rs (ProfileName), session.rs (SessionId), tool_call.rs (ToolCallId), request.rs (RequestId) — and invokes newtype_id! exactly once. AgentKind in agent/mod.rs is a plain enum { Pi, Claude } with serde derive (NOT a newtype) — variants serialize as 'pi'/'claude'. The macro must NOT emit derive(From) or derive(Into) (NF-8 forbids them); values must enter via new() so future per-family validation can be added without bypass paths."
}
