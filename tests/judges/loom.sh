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
