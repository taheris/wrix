use loom_core::agent::SessionOutcome;

/// Result of one agent invocation against a bead. The driver translates
/// session-level signals (JSONL `result/success`, non-zero process exit,
/// `LOOM_BLOCKED` / `LOOM_CLARIFY` markers) into one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// Agent finished cleanly (`LOOM_COMPLETE`).
    Success,

    /// Agent exited with a non-zero `SessionComplete` code or surfaced a
    /// recoverable failure body. The string carries the body the driver
    /// should inject into the next retry's prompt as `previous_failure`.
    Failure { error: String },

    /// Pre-flight infra failure (image load, container start) — `B::spawn`
    /// returned an error before the agent process produced any output.
    /// Bypasses retry and routes straight to `loom:blocked` per
    /// `specs/loom-harness.md` § "Verdict Gate · Infra failures bypass the
    /// gate".
    InfraPreflight { error: String },

    /// Mid-session infra failure (agent process exit non-zero, container
    /// OOM, IO errors). Eligible for one driver-memory retry per `loom run`
    /// invocation. A second mid-session failure inside the same
    /// `run_loop` invocation routes to `loom:blocked`.
    InfraMidSession { error: String },
}

/// Final state of one bead after retries have been exhausted (or the agent
/// succeeded on first try). Drives the bd-side cleanup: success → `bd close`,
/// clarified → `bd update --add-label loom:clarify`, blocked →
/// `bd update --add-label loom:blocked --notes <cause>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadResult {
    /// Bead succeeded — caller closes it.
    Done,

    /// Retries exhausted — caller flags the bead with `loom:clarify`.
    Clarified { last_error: String },

    /// Infra failure routed straight to `loom:blocked`. `cause` is the
    /// stable spec-table label (`infra-preflight` or `infra-repeated`)
    /// the driver writes into `bd update --notes`; `error` carries the
    /// raw failure body for human triage.
    Blocked { cause: String, error: String },
}

/// Output of one classified agent dispatch. The run-loop closure produces
/// this so [`super::runner::process_one_bead`] can route preflight vs
/// mid-session failures to the right verdict-gate path.
#[derive(Debug, Clone)]
pub enum SessionResult {
    /// `B::spawn` succeeded and the session reached `SessionComplete`.
    /// `exit_code` may still be non-zero (the agent decided to fail) — the
    /// caller distinguishes that from infra failures via the variant.
    Complete(SessionOutcome),

    /// `B::spawn` itself failed (image load, container start). No agent
    /// output exists; routes to `loom:blocked` cause `infra-preflight`.
    PreflightFailed { error: String },

    /// Spawn succeeded but the session terminated before
    /// `SessionComplete` — process EOF, IO error, OOM kill, etc. Eligible
    /// for one driver-memory retry per `loom run`.
    MidSessionFailed { error: String },
}
