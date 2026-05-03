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

    /// Agent needs human input; the driver applies the `loom:clarify`
    /// label and bails.
    Clarify { question: String },
}

const COMPLETE: &str = "LOOM_COMPLETE";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";

/// Scan the agent's combined output (or the `result` field of the final
/// stream-json line) for an exit signal.
///
/// Returns the **last** match — agents sometimes summarise their plan
/// before settling on a verdict, and we want the verdict, not the plan.
/// `None` means no signal was found and the caller should surface
/// [`super::TodoError::MissingExitSignal`].
///
/// `LOOM_BLOCKED` and `LOOM_CLARIFY` are bare markers — no trailing colon,
/// no trailing payload. The reason / question is read from the text
/// **before** the marker:
///
/// 1. If the marker is preceded by non-whitespace text on the same line
///    (e.g. `Final result: missing schema LOOM_BLOCKED`), that text is the
///    reason.
/// 2. Otherwise, the most recent non-empty line before the marker line is
///    the reason.
/// 3. If neither exists, the reason is empty.
pub fn parse_exit_signal(output: &str) -> Option<ExitSignal> {
    let lines: Vec<&str> = output.lines().collect();
    let mut last: Option<ExitSignal> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(reason) = reason_for(BLOCKED, line, &lines[..i]) {
            last = Some(ExitSignal::Blocked { reason });
            continue;
        }
        if let Some(question) = reason_for(CLARIFY, line, &lines[..i]) {
            last = Some(ExitSignal::Clarify { question });
            continue;
        }
        if line.contains(COMPLETE) {
            last = Some(ExitSignal::Complete);
        }
    }
    last
}

fn reason_for(marker: &str, line: &str, prior: &[&str]) -> Option<String> {
    let idx = line.find(marker)?;
    let same_line = line[..idx].trim();
    if !same_line.is_empty() {
        return Some(same_line.to_string());
    }
    for prev in prior.iter().rev() {
        let trimmed = prev.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    Some(String::new())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn complete_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("ok\nLOOM_COMPLETE\n"),
            Some(ExitSignal::Complete)
        );
    }

    #[test]
    fn blocked_carries_reason_from_prior_line() {
        let out = "doing things\nspec is missing the requirements section\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "spec is missing the requirements section");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_carries_question_from_prior_line() {
        let out = "should the migration be additive only?\nLOOM_CLARIFY";
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
        let out = "tentative\nLOOM_BLOCKED\nactually nevermind\nLOOM_COMPLETE";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }

    #[test]
    fn marker_recognized_inside_a_longer_line() {
        let out = "Final result: missing schema LOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "Final result: missing schema");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_between_reason_and_marker_are_skipped() {
        let out = "the actual reason\n\n\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert_eq!(reason, "the actual reason"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn marker_at_start_with_no_prior_lines_yields_empty_reason() {
        let out = "LOOM_BLOCKED";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert!(reason.is_empty()),
            other => panic!("expected Blocked with empty reason, got {other:?}"),
        }
    }
}
