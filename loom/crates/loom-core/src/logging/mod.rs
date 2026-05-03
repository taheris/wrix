//! Run-time event sink, terminal renderer, and log retention sweep.
//!
//! `loom run` produces a stream of [`AgentEvent`](crate::agent::AgentEvent)
//! values per bead. Each bead spawn opens a [`LogSink`] that tees the same
//! event stream into two destinations:
//!
//! 1. A per-bead NDJSON file under
//!    `<workspace>/.wrapix/loom/logs/<spec-label>/<bead-id>-<utc>.ndjson`.
//! 2. A [`TerminalRenderer`] that draws human-friendly progress lines on
//!    stdout (default) or full assistant text streams (`--verbose`).
//!
//! Both writers are driven from the same `LogSink::emit` call — there is no
//! independent renderer task pulling events in parallel — so the renderer and
//! the on-disk log are guaranteed to observe the same event sequence.
//!
//! [`Redacted`] wraps any value that may contain secrets so it logs as
//! `[REDACTED]` regardless of underlying content; variable *names* may still
//! appear in tracing fields.
//!
//! [`sweep_retention`] is invoked once per `loom run` startup to delete log
//! files older than the configured retention window. Failures (permission
//! denied, in-use file) are logged at `debug!` and do not abort the run.

mod error;
mod path;
mod redacted;
mod renderer;
mod retention;
mod sink;
mod time;

pub use error::LogError;
pub use path::{bead_log_path, format_utc_timestamp};
pub use redacted::Redacted;
pub use renderer::{BeadOutcome, RenderMode, TerminalRenderer};
pub use retention::{RetentionReport, sweep_retention, sweep_retention_at};
pub use sink::LogSink;
