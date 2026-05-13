//! `loom check` — post-loop reviewer + push gate.
//!
//! Implements the review-gate semantics defined in
//! `specs/ralph-review.md` ("Push gate" / "Auto-iteration loop") on top of
//! `loom-driver`'s typed surface and `loom-templates`' `check.md` template.
//! The gate:
//!
//! 1. snapshots beads carrying `spec:<label>` (`pre`);
//! 2. renders [`CheckContext`](loom_templates::check::CheckContext), spawns
//!    `wrapix spawn --spawn-config <file> --stdio`, drives an
//!    [`AgentBackend`](loom_driver::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_driver::agent::AgentEvent) stream into the
//!    terminal renderer + per-bead JSONL log;
//! 3. snapshots beads again, computes new bead IDs and clarify membership;
//! 4. branches: clean → `git push` + `beads-push`; clarify → stop;
//!    fix-up + under cap → `exec loom run`; fix-up + at cap → escalate the
//!    newest fix-up bead to `loom:clarify`.
//!
//! `loom run`'s auto-check handoff (`exec_check` in [`super::run`]) is
//! wired by the binary to invoke this module.

mod context;
mod error;
mod iteration;
mod phase_verdict;
mod production;
mod runner;
mod verdict;
mod verify_fail;

pub use context::{CheckContextInputs, beads_summary, build_check_context, load_review_sources};
pub use error::CheckError;
pub use iteration::{DEFAULT_MAX_ITERATIONS, IterationCap};
pub use phase_verdict::{
    GateInputs, PhaseVerdict, RecoveryCause, ReviewConcern, ReviewFlag, decide, parse_review_flag,
};
pub use production::ProductionCheckController;
pub use runner::{CheckController, CheckResult, ReviewOutcome, check_loop};
pub use verdict::{BeadSnapshot, CheckVerdict, diff_new_bead_ids};
pub use verify_fail::{
    PREVIOUS_FAILURE_BUDGET, REVIEW_NOTES_BUDGET, STDERR_TAIL_LINES, VerifyFailure,
    format_previous_failure,
};
