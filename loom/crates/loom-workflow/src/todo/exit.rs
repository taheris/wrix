/// Parsed exit signal from an agent session — the trailing line the agent
/// emits to signal whether `loom todo` should advance or roll back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitSignal {
    /// Agent finished cleanly; the driver advances per-spec cursors and
    /// commits the spec file.
    Complete,

    /// Agent could not proceed; the driver surfaces the reason to the user
    /// without advancing state.
    Blocked { reason: String },

    /// Agent needs human input; the driver applies the `ralph:clarify`
    /// label and bails.
    Clarify { question: String },
}

const COMPLETE: &str = "RALPH_COMPLETE";
const BLOCKED: &str = "RALPH_BLOCKED:";
const CLARIFY: &str = "RALPH_CLARIFY:";

/// Scan the agent's combined output (or the `result` field of the final
/// stream-json line) for an exit signal.
///
/// Returns the **last** match — agents sometimes summarise their plan
/// before settling on a verdict, and we want the verdict, not the plan.
/// `None` means no signal was found and the caller should surface
/// [`super::TodoError::MissingExitSignal`].
///
/// The matchers are token-based, not exact-line: an agent that prints
/// `Final result: RALPH_BLOCKED: missing schema` is treated the same way as
/// a bare `RALPH_BLOCKED: missing schema` line. Reason text starts at the
/// first non-space byte after the marker and runs to end-of-line.
pub fn parse_exit_signal(output: &str) -> Option<ExitSignal> {
    let mut last: Option<ExitSignal> = None;
    for line in output.lines() {
        if let Some(reason) = find_after(line, BLOCKED) {
            last = Some(ExitSignal::Blocked {
                reason: reason.to_string(),
            });
            continue;
        }
        if let Some(question) = find_after(line, CLARIFY) {
            last = Some(ExitSignal::Clarify {
                question: question.to_string(),
            });
            continue;
        }
        if line.contains(COMPLETE) {
            last = Some(ExitSignal::Complete);
        }
    }
    last
}

fn find_after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let idx = line.find(marker)?;
    let tail = &line[idx + marker.len()..];
    Some(tail.trim())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn complete_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("ok\nRALPH_COMPLETE\n"),
            Some(ExitSignal::Complete)
        );
    }

    #[test]
    fn blocked_carries_reason_after_marker() {
        let out = "doing things\nRALPH_BLOCKED: spec is missing the requirements section\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "spec is missing the requirements section");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_carries_question() {
        let out = "RALPH_CLARIFY: should the migration be additive only?";
        match parse_exit_signal(out) {
            Some(ExitSignal::Clarify { question }) => {
                assert_eq!(question, "should the migration be additive only?");
            }
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn no_signal_returns_none() {
        assert_eq!(
            parse_exit_signal("just some output\nno marker here\n"),
            None
        );
    }

    #[test]
    fn last_match_wins_when_multiple_present() {
        // Agent first declares blocked, then changes its mind and finishes.
        let out = "RALPH_BLOCKED: tentative\nactually nevermind\nRALPH_COMPLETE";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }

    #[test]
    fn marker_recognized_inside_a_longer_line() {
        let out = "Final result: RALPH_BLOCKED: missing schema\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }
}
