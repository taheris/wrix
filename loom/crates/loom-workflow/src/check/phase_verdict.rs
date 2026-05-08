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

use super::verify_fail::VerifyFailure;
use crate::todo::ExitSignal;

/// Which concern in the review LLM's structured response triggered the flag.
/// Mirrors the four concerns the spec enumerates (`specs/loom-harness.md`
/// §"Push gate · Review always runs"): live-path coverage, mock discipline,
/// scope appropriateness, and `[judge]` rubric satisfaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewConcern {
    LivePath,
    Mock,
    Scope,
    Judge,
}

impl ReviewConcern {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LivePath => "live-path",
            Self::Mock => "mock",
            Self::Scope => "scope",
            Self::Judge => "judge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "live-path" => Some(Self::LivePath),
            "mock" => Some(Self::Mock),
            "scope" => Some(Self::Scope),
            "judge" => Some(Self::Judge),
            _ => None,
        }
    }
}

/// Parsed contents of the review LLM's structured flag emission. The detail
/// string carried here is what feeds the `review-flag` row of
/// `previous_failure` (`specs/loom-harness.md` §"Recovery context") — sourced
/// from the structured emission, not regex-extracted from prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFlag {
    pub concern: ReviewConcern,
    pub detail: String,
}

/// Why the gate routes to recovery. Mirrors the cause strings in the spec
/// table so they show up unchanged in `bd update --notes` when retries are
/// exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryCause {
    /// No exit marker found in the agent output.
    SwallowedMarker,
    /// Marker was emitted but the bead was not bd-closed.
    IncompleteSignaling,
    /// `LOOM_COMPLETE` with an empty worktree diff. `LOOM_NOOP` is the
    /// legitimate path for an empty diff and never produces this cause.
    ZeroProgress,
    /// At least one `[verify]` script failed. Carries every failure so the
    /// downstream `previous_failure` builder can format them into a single
    /// budget-bounded body — none short-circuit each other.
    VerifyFail { failures: Vec<VerifyFailure> },
    /// Verify passed but the reviewer raised a flag. Carries the structured
    /// concern + reasoning emitted by the review LLM so downstream surfaces
    /// (`bd update --notes`, `previous_failure`) can name which concern
    /// triggered without re-parsing the agent's prose.
    ReviewFlag(ReviewFlag),
}

impl RecoveryCause {
    /// Stable spec-table label used in user-facing surfaces (logs, bd notes).
    /// The label is the same for every review-flag concern; per-concern
    /// detail lives in [`RecoveryCause::ReviewFlag`]'s payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SwallowedMarker => "swallowed-marker",
            Self::IncompleteSignaling => "incomplete-signaling",
            Self::ZeroProgress => "zero-progress",
            Self::VerifyFail { .. } => "verify-fail",
            Self::ReviewFlag(_) => "review-flag",
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateInputs {
    /// Bead carries `closed` status after the phase ran.
    pub bd_closed: bool,
    /// `git diff` against the driver branch produced no output.
    pub diff_empty: bool,
    /// Failure record for every `[verify]` script that exited non-zero.
    /// Empty when every script passed; the gate routes to
    /// [`RecoveryCause::VerifyFail`] when this is non-empty and threads the
    /// list through so downstream surfaces can format `previous_failure`.
    pub verify_failures: Vec<VerifyFailure>,
    /// Parsed reviewer flag, or `None` for a clean review.
    pub review_flag: Option<ReviewFlag>,
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
    if !inputs.verify_failures.is_empty() {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::VerifyFail {
                failures: inputs.verify_failures,
            },
        };
    }
    if let Some(flag) = inputs.review_flag {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewFlag(flag),
        };
    }
    PhaseVerdict::Done
}

/// Marker prefix the review LLM emits to flag a concern. Format:
///
/// ```text
/// LOOM_REVIEW_FLAG: <concern> -- <detail>
/// ```
///
/// `<concern>` is one of `live-path`, `mock`, `scope`, `judge`. `<detail>`
/// is free-form one-line reasoning. The marker lives on its own line; if the
/// LLM emits it multiple times only the last well-formed occurrence wins,
/// matching [`crate::todo::parse_exit_signal`]'s last-match policy.
const REVIEW_FLAG_MARKER: &str = "LOOM_REVIEW_FLAG:";
const REVIEW_FLAG_SEPARATOR: &str = "--";

/// Parse the review LLM's structured flag emission from its combined output.
/// Returns `None` for review-pass (no marker, or marker present but the
/// concern token is not one of the four enum values).
///
/// This is the **only** path by which a `RecoveryCause::ReviewFlag` detail
/// is produced — the spec ([§"Recovery context"](specs/loom-harness.md))
/// requires the detail come from the structured emission, not regex-pulled
/// from surrounding prose.
pub fn parse_review_flag(output: &str) -> Option<ReviewFlag> {
    let mut last: Option<ReviewFlag> = None;
    for line in output.lines() {
        let Some(idx) = line.find(REVIEW_FLAG_MARKER) else {
            continue;
        };
        let after = line[idx + REVIEW_FLAG_MARKER.len()..].trim();
        let (concern_str, detail) = match after.split_once(REVIEW_FLAG_SEPARATOR) {
            Some((c, d)) => (c.trim(), d.trim().to_string()),
            None => (after, String::new()),
        };
        if let Some(concern) = ReviewConcern::parse(concern_str) {
            last = Some(ReviewFlag { concern, detail });
        }
    }
    last
}

#[cfg(test)]
#[expect(
    clippy::panic,
    clippy::expect_used,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    fn inputs(
        bd_closed: bool,
        diff_empty: bool,
        verify_pass: bool,
        review_flag: Option<ReviewFlag>,
    ) -> GateInputs {
        let verify_failures = if verify_pass {
            Vec::new()
        } else {
            vec![sample_failure()]
        };
        GateInputs {
            bd_closed,
            diff_empty,
            verify_failures,
            review_flag,
        }
    }

    fn sample_failure() -> VerifyFailure {
        VerifyFailure {
            script_path: std::path::PathBuf::from("tests/sample.sh"),
            exit_code: 1,
            stderr: "boom\n".into(),
        }
    }

    fn flag(concern: ReviewConcern, detail: &str) -> ReviewFlag {
        ReviewFlag {
            concern,
            detail: detail.to_string(),
        }
    }

    // --- Marker-only rows (bd/diff/review irrelevant). ---

    #[test]
    fn blocked_marker_routes_to_blocked_with_reason() {
        let m = ExitSignal::Blocked {
            reason: "missing schema".into(),
        };
        match decide(
            Some(&m),
            inputs(false, true, false, Some(flag(ReviewConcern::Mock, "x"))),
        ) {
            PhaseVerdict::Blocked { reason } => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_marker_routes_to_clarify_with_question() {
        let m = ExitSignal::Clarify {
            question: "additive only?".into(),
        };
        match decide(Some(&m), inputs(true, false, true, None)) {
            PhaseVerdict::Clarify { question } => assert_eq!(question, "additive only?"),
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn missing_marker_routes_to_swallowed_marker_recovery() {
        assert_eq!(
            decide(None, inputs(true, false, true, None)),
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
                inputs(false, false, true, None)
            ),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn complete_with_empty_diff_routes_to_zero_progress() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, true, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ZeroProgress,
            },
        );
    }

    #[test]
    fn complete_with_verify_fail_routes_to_verify_fail() {
        let result = decide(
            Some(&ExitSignal::Complete),
            inputs(true, false, false, None),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::VerifyFail { failures },
            } => {
                assert_eq!(failures.len(), 1, "carries every failure block");
                assert_eq!(failures[0].exit_code, 1);
            }
            other => panic!("expected Recovery::VerifyFail, got {other:?}"),
        }
    }

    #[test]
    fn verify_fail_carries_every_failure_block_for_previous_failure() {
        // Spec gate: `previous_failure` carries every failure (not just the
        // first). The recovery-cause payload is the channel — downstream
        // formatter splits the 4000-char budget across them.
        let failures = vec![
            VerifyFailure {
                script_path: std::path::PathBuf::from("tests/a.sh"),
                exit_code: 1,
                stderr: "boom-a".into(),
            },
            VerifyFailure {
                script_path: std::path::PathBuf::from("tests/b.sh"),
                exit_code: 2,
                stderr: "boom-b".into(),
            },
        ];
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            verify_failures: failures.clone(),
            review_flag: None,
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::VerifyFail { failures: carried },
            } => {
                assert_eq!(carried, failures, "every failure threaded through");
            }
            other => panic!("expected Recovery::VerifyFail, got {other:?}"),
        }
    }

    #[test]
    fn complete_with_review_flag_routes_to_review_flag() {
        let detail = "test mocks the agent backend instead of spawning it";
        let result = decide(
            Some(&ExitSignal::Complete),
            inputs(
                true,
                false,
                true,
                Some(flag(ReviewConcern::LivePath, detail)),
            ),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewFlag(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::LivePath);
                assert_eq!(parsed.detail, detail);
            }
            other => panic!("expected Recovery::ReviewFlag, got {other:?}"),
        }
    }

    #[test]
    fn complete_clean_routes_to_done() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, false, true, None)),
            PhaseVerdict::Done,
        );
    }

    // --- LOOM_NOOP rows (the four scoped by this bead). ---

    #[test]
    fn noop_without_bd_closed_routes_to_incomplete_signaling() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(false, true, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn noop_with_verify_fail_routes_to_verify_fail() {
        // Empty diff allowed under Noop; verify failure still recovers.
        for diff_empty in [true, false] {
            let result = decide(
                Some(&ExitSignal::Noop),
                inputs(true, diff_empty, false, None),
            );
            match result {
                PhaseVerdict::Recovery {
                    cause: RecoveryCause::VerifyFail { failures },
                } => {
                    assert_eq!(failures.len(), 1, "diff_empty={diff_empty}");
                }
                other => panic!("expected VerifyFail (diff_empty={diff_empty}), got {other:?}"),
            }
        }
    }

    #[test]
    fn noop_with_review_flag_routes_to_review_flag() {
        let detail = "diff edits files outside the spec's Affected Files list";
        let result = decide(
            Some(&ExitSignal::Noop),
            inputs(true, true, true, Some(flag(ReviewConcern::Scope, detail))),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewFlag(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::Scope);
                assert_eq!(parsed.detail, detail);
            }
            other => panic!("expected Recovery::ReviewFlag, got {other:?}"),
        }
    }

    #[test]
    fn noop_with_empty_diff_and_clean_review_is_done_not_zero_progress() {
        // The reason this gate exists: empty diff + Noop must NOT trip
        // zero-progress recovery — the work was already in tree.
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, true, None)),
            PhaseVerdict::Done,
        );
    }

    #[test]
    fn noop_with_non_empty_diff_and_clean_review_is_done() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, false, true, None)),
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
        assert_eq!(
            RecoveryCause::VerifyFail { failures: vec![] }.as_str(),
            "verify-fail",
        );
        assert_eq!(
            RecoveryCause::ReviewFlag(flag(ReviewConcern::Judge, "")).as_str(),
            "review-flag",
        );
    }

    #[test]
    fn review_concern_labels_match_spec_vocabulary() {
        assert_eq!(ReviewConcern::LivePath.as_str(), "live-path");
        assert_eq!(ReviewConcern::Mock.as_str(), "mock");
        assert_eq!(ReviewConcern::Scope.as_str(), "scope");
        assert_eq!(ReviewConcern::Judge.as_str(), "judge");
    }

    #[test]
    fn review_concern_parse_round_trips_each_variant() {
        for c in [
            ReviewConcern::LivePath,
            ReviewConcern::Mock,
            ReviewConcern::Scope,
            ReviewConcern::Judge,
        ] {
            assert_eq!(ReviewConcern::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn review_concern_parse_rejects_unknown_token() {
        assert_eq!(ReviewConcern::parse("livepath"), None);
        assert_eq!(ReviewConcern::parse("nit"), None);
        assert_eq!(ReviewConcern::parse(""), None);
    }

    // --- Structured flag parsing. ---

    #[test]
    fn parse_review_flag_returns_none_when_marker_absent() {
        assert!(parse_review_flag("LOOM_COMPLETE\n").is_none());
        assert!(parse_review_flag("ok\nno flag here\n").is_none());
    }

    #[test]
    fn parse_review_flag_extracts_concern_and_detail_from_marker() {
        let out = "preamble\nLOOM_REVIEW_FLAG: live-path -- test mocks the agent backend\nLOOM_COMPLETE\n";
        let parsed = parse_review_flag(out).expect("flag parsed");
        assert_eq!(parsed.concern, ReviewConcern::LivePath);
        assert_eq!(parsed.detail, "test mocks the agent backend");
    }

    #[test]
    fn parse_review_flag_supports_each_concern_variant() {
        for (token, expected) in [
            ("live-path", ReviewConcern::LivePath),
            ("mock", ReviewConcern::Mock),
            ("scope", ReviewConcern::Scope),
            ("judge", ReviewConcern::Judge),
        ] {
            let out = format!("LOOM_REVIEW_FLAG: {token} -- because\n");
            let parsed = parse_review_flag(&out).expect("flag parsed");
            assert_eq!(parsed.concern, expected);
            assert_eq!(parsed.detail, "because");
        }
    }

    #[test]
    fn parse_review_flag_takes_last_well_formed_match() {
        let out = "LOOM_REVIEW_FLAG: mock -- first\n\
                   LOOM_REVIEW_FLAG: judge -- second\n";
        let parsed = parse_review_flag(out).expect("flag parsed");
        assert_eq!(parsed.concern, ReviewConcern::Judge);
        assert_eq!(parsed.detail, "second");
    }

    #[test]
    fn parse_review_flag_skips_marker_with_unknown_concern() {
        // A garbled marker collapses to "no flag" — better than synthesising
        // a bogus concern that downstream surfaces would print verbatim.
        assert!(parse_review_flag("LOOM_REVIEW_FLAG: nit -- whatever\n").is_none());
    }

    #[test]
    fn parse_review_flag_accepts_empty_detail() {
        let parsed =
            parse_review_flag("LOOM_REVIEW_FLAG: scope\n").expect("concern-only marker parses");
        assert_eq!(parsed.concern, ReviewConcern::Scope);
        assert!(parsed.detail.is_empty());
    }
}
