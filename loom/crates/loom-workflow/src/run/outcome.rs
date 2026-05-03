/// Result of one agent invocation against a bead. The driver translates
/// session-level signals (NDJSON `result/success`, non-zero process exit,
/// `RALPH_BLOCKED` / `RALPH_CLARIFY` markers) into one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// Agent finished cleanly (`RALPH_COMPLETE`).
    Success,

    /// Agent exited non-clean — either crashed, ran out of budget, or emitted
    /// `RALPH_BLOCKED`. The string carries the body the driver should inject
    /// into the next retry's prompt as `previous_failure`.
    Failure { error: String },
}

/// Final state of one bead after retries have been exhausted (or the agent
/// succeeded on first try). Drives the bd-side cleanup: success → `bd close`,
/// clarified → `bd update --add-label ralph:clarify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadResult {
    /// Bead succeeded — caller closes it.
    Done,

    /// Retries exhausted — caller flags the bead with `ralph:clarify`.
    Clarified { last_error: String },
}
