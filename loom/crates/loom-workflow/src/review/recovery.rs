//! Verdict-gate recovery resolution (`specs/loom-harness.md` lines
//! 829-832 + 906-918 + 1960-1981).
//!
//! When the verdict gate's decision table produces
//! [`PhaseVerdict::Recovery`](super::phase_verdict::PhaseVerdict::Recovery)
//! the driver maps that onto one of two terminal shapes against the
//! bead's iteration counter and `[loop] max_iterations`:
//!
//! - **Retry** — iter `<` max. The original recovery cause is rendered
//!   into the next session's `previous_failure` payload (see
//!   [`super::verify_fail::format_previous_failure`]) and the bead is
//!   re-dispatched.
//! - **Blocked** — iter `>=` max. The driver applies `loom:blocked` with
//!   cause `retry-exhausted`; the original recovery cause is preserved
//!   in `bd update --notes` so the human can see *why* the loop gave up.
//!
//! The "spawns fix-up bead OR retries" disjunction in the spec criterion
//! `test_recovery_under_max` is satisfied by this module's Retry branch
//! plus the dedicated chokepoint in [`super::fixup`]: callers that want
//! to spawn a fix-up bead during a Retry pass invoke
//! [`super::fixup::spawn_fixup_bead`] explicitly.

use super::phase_verdict::RecoveryCause;
use super::verify_fail::format_previous_failure;

/// Render a [`RecoveryCause`] into a `previous_failure` body suitable for
/// threading into the next agent attempt's prompt — or into the blocked
/// notes when retries exhaust. Mirrors the per-cause detail table in
/// `specs/loom-harness.md` §"Recovery context (`previous_failure`)".
fn render_previous_failure(cause: &RecoveryCause) -> String {
    match cause {
        RecoveryCause::SwallowedMarker => {
            "Last phase ended without a `LOOM_*` exit marker.".to_string()
        }
        RecoveryCause::IncompleteSignaling => {
            "Marker `LOOM_COMPLETE` emitted but the bead was not bd-closed.".to_string()
        }
        RecoveryCause::ZeroProgress => {
            "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed."
                .to_string()
        }
        RecoveryCause::VerifyFail {
            failures,
            review_notes,
        } => format_previous_failure(failures, review_notes.as_ref()),
        RecoveryCause::ReviewFlag(flag) => {
            format!("[{}] {}", flag.concern.as_str(), flag.detail)
        }
    }
}

/// Cause string written to `bd update --notes` when the recovery loop
/// exhausts `[loop] max_iterations`. Mirrored from
/// `specs/loom-harness.md` §"Verdict Gate · Labels".
pub const RETRY_EXHAUSTED_CAUSE: &str = "retry-exhausted";

/// Resolution of one [`RecoveryCause`] against the bead's iteration
/// counter + `[loop] max_iterations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResolution {
    /// Iter `<` max. Re-dispatch the bead with `previous_failure`
    /// threaded into the next session's prompt. The body is the spec's
    /// recovery-cause formatting (`super::verify_fail::format_previous_failure`).
    Retry { previous_failure: String },
    /// Iter `>=` max. Apply `loom:blocked` + `retry-exhausted` to the
    /// bead. `notes` is the `bd update --notes` body — it leads with the
    /// `retry-exhausted` label, names the original recovery cause for
    /// human review, and embeds the same `previous_failure` body the
    /// last retry would have seen (so the surface tells the human what
    /// the agent kept failing on).
    Blocked { cause: String, notes: String },
}

/// Pure-ish recovery resolver. `max` is `[loop] max_iterations` (default
/// 3). `iter` is the bead's pre-decision iteration counter: 0 on the
/// first failure, so the function returns Retry until `iter` reaches
/// `max`.
pub fn resolve_recovery(cause: &RecoveryCause, iter: u32, max: u32) -> RecoveryResolution {
    let previous_failure = render_previous_failure(cause);
    if iter >= max {
        let notes = format!(
            "{RETRY_EXHAUSTED_CAUSE}: recovery loop hit [loop] max_iterations ({max}) on cause `{original}`.\n\n{previous_failure}",
            original = cause.as_str(),
        );
        RecoveryResolution::Blocked {
            cause: RETRY_EXHAUSTED_CAUSE.to_string(),
            notes,
        }
    } else {
        RecoveryResolution::Retry { previous_failure }
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use crate::review::phase_verdict::{ReviewConcern, ReviewFlag};
    use crate::review::verify_fail::VerifyFailure;

    fn verify_fail_cause() -> RecoveryCause {
        RecoveryCause::VerifyFail {
            failures: vec![VerifyFailure {
                script_path: std::path::PathBuf::from("tests/sample.sh"),
                exit_code: 1,
                stderr: "boom\n".into(),
            }],
            review_notes: None,
        }
    }

    #[test]
    fn under_max_recovers_with_previous_failure() {
        // iter < max → Retry, and the `previous_failure` body must be the
        // spec-formatted cause body so the next session's prompt sees
        // every failing-verify block (and any threaded review notes).
        let cause = verify_fail_cause();
        match resolve_recovery(&cause, 0, 3) {
            RecoveryResolution::Retry { previous_failure } => {
                assert!(
                    previous_failure.contains("tests/sample.sh"),
                    "previous_failure carries the spec-formatted cause body: {previous_failure}",
                );
                assert!(
                    previous_failure.contains("boom"),
                    "stderr tail is part of the previous_failure body: {previous_failure}",
                );
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn under_max_at_two_still_recovers_with_previous_failure() {
        // The boundary is strict-less-than: iter=2 with max=3 still
        // retries (one slot left). iter=3 is exhausted (see next test).
        let cause = RecoveryCause::SwallowedMarker;
        let res = resolve_recovery(&cause, 2, 3);
        assert!(matches!(res, RecoveryResolution::Retry { .. }));
    }

    #[test]
    fn at_or_above_max_applies_blocked_with_retry_exhausted_cause() {
        // iter >= max → Blocked. The cause is the literal spec string
        // `retry-exhausted`; the notes preserve the original recovery
        // cause so the human reading `bd show --notes` knows what kept
        // failing.
        let cause = verify_fail_cause();
        for iter in [3u32, 4, 99] {
            match resolve_recovery(&cause, iter, 3) {
                RecoveryResolution::Blocked {
                    cause: applied,
                    notes,
                } => {
                    assert_eq!(applied, RETRY_EXHAUSTED_CAUSE);
                    assert_eq!(applied, "retry-exhausted");
                    assert!(
                        notes.contains("retry-exhausted"),
                        "notes lead with the retry-exhausted label: {notes}",
                    );
                    assert!(
                        notes.contains("verify-fail"),
                        "notes preserve original cause for human review: {notes}",
                    );
                    assert!(
                        notes.contains("max_iterations (3)"),
                        "notes cite the cap that was hit: {notes}",
                    );
                    assert!(
                        notes.contains("tests/sample.sh"),
                        "previous_failure body is embedded in notes: {notes}",
                    );
                    assert!(
                        notes.contains("boom"),
                        "stderr tail is embedded in notes: {notes}",
                    );
                }
                other => panic!("expected Blocked at iter={iter}, got {other:?}"),
            }
        }
    }

    #[test]
    fn review_flag_cause_round_trips_through_notes() {
        // The `review-flag` cause carries the concern + verbatim flag
        // detail; both must survive into the blocked notes when the
        // recovery loop gives up on a flag.
        let cause = RecoveryCause::ReviewFlag(ReviewFlag {
            concern: ReviewConcern::LivePath,
            detail: "test mocks the agent backend".into(),
        });
        match resolve_recovery(&cause, 3, 3) {
            RecoveryResolution::Blocked { cause, notes } => {
                assert_eq!(cause, "retry-exhausted");
                assert!(notes.contains("review-flag"), "notes name original cause");
                assert!(
                    notes.contains("test mocks the agent backend"),
                    "review reasoning is preserved verbatim: {notes}",
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn zero_max_exhausts_immediately() {
        // Degenerate `max=0` (no retries allowed at all) immediately
        // exhausts — the first failure is also the terminal one.
        let cause = RecoveryCause::SwallowedMarker;
        assert!(matches!(
            resolve_recovery(&cause, 0, 0),
            RecoveryResolution::Blocked { .. }
        ));
    }
}
