//! Per-phase verdict gate (`specs/loom-harness.md` lines 444-470).
//!
//! After every agent phase ends, `loom check` evaluates the result through
//! this deterministic gate before the bead's state can advance. The gate
//! combines four mechanical/agent-judged signals — exit marker, bd-closed,
//! diff emptiness, and the review verdict — into one of `done`, `blocked`,
//! `clarify`, or `recovery` with a typed cause.
//!
//! Logic is a pure function of the four signals; the binary owns the
//! plumbing that produces them and the recovery-loop dispatch on the other
//! side.

use crate::todo::ExitSignal;

/// Why the gate routes to recovery. Mirrors the cause strings in the spec
/// table so they show up unchanged in `bd update --notes` when retries are
/// exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryCause {
    /// No exit marker found in the agent output.
    SwallowedMarker,
    /// Marker was emitted but the bead was not bd-closed.
    IncompleteSignaling,
    /// `LOOM_COMPLETE` with an empty worktree diff. `LOOM_NOOP` is the
    /// legitimate path for an empty diff and never produces this cause.
    ZeroProgress,
    /// At least one `[verify]` script failed.
    VerifyFail,
    /// Verify passed but the reviewer raised a flag.
    ReviewFlag,
}

impl RecoveryCause {
    /// Stable spec-table label used in user-facing surfaces (logs, bd notes).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SwallowedMarker => "swallowed-marker",
            Self::IncompleteSignaling => "incomplete-signaling",
            Self::ZeroProgress => "zero-progress",
            Self::VerifyFail => "verify-fail",
            Self::ReviewFlag => "review-flag",
        }
    }
}

/// One of the four post-gate branches. The driver maps `Recovery` onto
/// `retry` (under `[loop] max_iterations`) or `blocked` (cap exhausted) one
/// layer up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseVerdict {
    /// Phase passed every gate stage — caller advances state.
    Done,
    /// Agent emitted `LOOM_BLOCKED` — surface to user without retry.
    Blocked { reason: String },
    /// Agent emitted `LOOM_CLARIFY` — apply `loom:clarify` and stop.
    Clarify { question: String },
    /// Mechanical or review failure — caller resolves to retry/blocked
    /// against the iteration counter.
    Recovery { cause: RecoveryCause },
}

/// Mechanical inputs the gate consumes alongside the parsed exit marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateInputs {
    /// Bead carries `closed` status after the phase ran.
    pub bd_closed: bool,
    /// `git diff` against the driver branch produced no output.
    pub diff_empty: bool,
    /// Every attached `[verify]` script exited 0.
    pub verify_pass: bool,
    /// Reviewer raised one or more findings.
    pub review_flag: bool,
}

/// Apply the spec's decision table to the parsed marker plus mechanical
/// signals. `marker = None` means no exit marker was found in the agent
/// output (translated from [`crate::todo::parse_exit_signal`] returning
/// `None`).
pub fn decide(marker: Option<&ExitSignal>, inputs: GateInputs) -> PhaseVerdict {
    match marker {
        None => PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        },
        Some(ExitSignal::Blocked { reason }) => PhaseVerdict::Blocked {
            reason: reason.clone(),
        },
        Some(ExitSignal::Clarify { question }) => PhaseVerdict::Clarify {
            question: question.clone(),
        },
        Some(ExitSignal::Complete) => decide_progress_marker(false, inputs),
        Some(ExitSignal::Noop) => decide_progress_marker(true, inputs),
    }
}

/// Branch shared by `LOOM_COMPLETE` and `LOOM_NOOP`: both require the bead
/// to be closed and both gate on verify+review. They differ only in how an
/// empty diff is treated — Complete demands non-empty, Noop accepts any.
fn decide_progress_marker(is_noop: bool, inputs: GateInputs) -> PhaseVerdict {
    if !inputs.bd_closed {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::IncompleteSignaling,
        };
    }
    if !is_noop && inputs.diff_empty {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ZeroProgress,
        };
    }
    if !inputs.verify_pass {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::VerifyFail,
        };
    }
    if inputs.review_flag {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewFlag,
        };
    }
    PhaseVerdict::Done
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests use panicking helpers")]
mod tests {
    use super::*;

    fn inputs(
        bd_closed: bool,
        diff_empty: bool,
        verify_pass: bool,
        review_flag: bool,
    ) -> GateInputs {
        GateInputs {
            bd_closed,
            diff_empty,
            verify_pass,
            review_flag,
        }
    }

    // --- Marker-only rows (bd/diff/review irrelevant). ---

    #[test]
    fn blocked_marker_routes_to_blocked_with_reason() {
        let m = ExitSignal::Blocked {
            reason: "missing schema".into(),
        };
        match decide(Some(&m), inputs(false, true, false, true)) {
            PhaseVerdict::Blocked { reason } => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_marker_routes_to_clarify_with_question() {
        let m = ExitSignal::Clarify {
            question: "additive only?".into(),
        };
        match decide(Some(&m), inputs(true, false, true, false)) {
            PhaseVerdict::Clarify { question } => assert_eq!(question, "additive only?"),
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn missing_marker_routes_to_swallowed_marker_recovery() {
        assert_eq!(
            decide(None, inputs(true, false, true, false)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::SwallowedMarker,
            },
        );
    }

    // --- LOOM_COMPLETE rows. ---

    #[test]
    fn complete_without_bd_closed_routes_to_incomplete_signaling() {
        assert_eq!(
            decide(
                Some(&ExitSignal::Complete),
                inputs(false, false, true, false)
            ),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn complete_with_empty_diff_routes_to_zero_progress() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, true, true, false)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ZeroProgress,
            },
        );
    }

    #[test]
    fn complete_with_verify_fail_routes_to_verify_fail() {
        assert_eq!(
            decide(
                Some(&ExitSignal::Complete),
                inputs(true, false, false, false)
            ),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::VerifyFail,
            },
        );
    }

    #[test]
    fn complete_with_review_flag_routes_to_review_flag() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, false, true, true)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewFlag,
            },
        );
    }

    #[test]
    fn complete_clean_routes_to_done() {
        assert_eq!(
            decide(
                Some(&ExitSignal::Complete),
                inputs(true, false, true, false)
            ),
            PhaseVerdict::Done,
        );
    }

    // --- LOOM_NOOP rows (the four scoped by this bead). ---

    #[test]
    fn noop_without_bd_closed_routes_to_incomplete_signaling() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(false, true, true, false)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn noop_with_verify_fail_routes_to_verify_fail() {
        // Empty diff allowed under Noop; verify failure still recovers.
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, false, false)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::VerifyFail,
            },
        );
        // Non-empty diff with verify-fail also recovers.
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, false, false, false)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::VerifyFail,
            },
        );
    }

    #[test]
    fn noop_with_review_flag_routes_to_review_flag() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, true, true)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewFlag,
            },
        );
    }

    #[test]
    fn noop_with_empty_diff_and_clean_review_is_done_not_zero_progress() {
        // The reason this gate exists: empty diff + Noop must NOT trip
        // zero-progress recovery — the work was already in tree.
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, true, false)),
            PhaseVerdict::Done,
        );
    }

    #[test]
    fn noop_with_non_empty_diff_and_clean_review_is_done() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, false, true, false)),
            PhaseVerdict::Done,
        );
    }

    // --- Cause label round-trip (bd notes / log surfaces). ---

    #[test]
    fn recovery_cause_labels_match_spec_strings() {
        assert_eq!(RecoveryCause::SwallowedMarker.as_str(), "swallowed-marker");
        assert_eq!(
            RecoveryCause::IncompleteSignaling.as_str(),
            "incomplete-signaling",
        );
        assert_eq!(RecoveryCause::ZeroProgress.as_str(), "zero-progress");
        assert_eq!(RecoveryCause::VerifyFail.as_str(), "verify-fail");
        assert_eq!(RecoveryCause::ReviewFlag.as_str(), "review-flag");
    }
}
