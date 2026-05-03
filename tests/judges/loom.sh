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
