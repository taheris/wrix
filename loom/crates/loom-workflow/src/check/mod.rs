//! `loom check` — post-loop reviewer + push gate.
//!
//! Implements the review-gate semantics defined in
//! `specs/ralph-review.md` ("Push gate" / "Auto-iteration loop") on top of
//! `loom-core`'s typed surface and `loom-templates`' `check.md` template.
//! The gate:
//!
//! 1. snapshots beads carrying `spec:<label>` (`pre`);
//! 2. renders [`CheckContext`](loom_templates::check::CheckContext), spawns
//!    `wrapix run-bead --spawn-config <file> --stdio`, drives an
//!    [`AgentBackend`](loom_core::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_core::agent::AgentEvent) stream into the
//!    terminal renderer + per-bead NDJSON log;
//! 3. snapshots beads again, computes new bead IDs and clarify membership;
//! 4. branches: clean → `git push` + `beads-push`; clarify → stop;
//!    fix-up + under cap → `exec loom run`; fix-up + at cap → escalate the
//!    newest fix-up bead to `ralph:clarify`.
//!
//! `loom run`'s auto-check handoff (`exec_check` in [`super::run`]) is
//! wired by the binary to invoke this module.

mod context;
mod error;
mod iteration;
mod runner;
mod verdict;

pub use context::{CheckContextInputs, beads_summary, build_check_context};
pub use error::CheckError;
pub use iteration::{DEFAULT_MAX_ITERATIONS, IterationCap};
pub use runner::{CheckController, CheckResult, ReviewOutcome, check_loop};
pub use verdict::{BeadSnapshot, CheckVerdict, diff_new_bead_ids};
