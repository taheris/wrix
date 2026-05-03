use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

/// Errors raised by the `loom todo` driver.
#[derive(Debug, Display, Error)]
pub enum TodoError {
    /// the `--since {commit}` override does not refer to a reachable commit
    InvalidSinceCommit { commit: String },

    /// agent supplied no exit signal — neither RALPH_COMPLETE nor RALPH_BLOCKED observed before session ended
    MissingExitSignal,

    /// agent reported RALPH_BLOCKED: {reason}
    AgentBlocked { reason: String },

    /// could not read spec file at {path}
    ReadSpec {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// rendering the prompt template failed
    Render(#[from] askama::Error),

    /// io operation failed
    Io(#[from] std::io::Error),
}
