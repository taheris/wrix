use displaydoc::Display;
use thiserror::Error;

use loom_core::agent::ProtocolError;
use loom_core::bd::BdError;
use loom_core::git::GitError;
use loom_core::logging::LogError;

/// Errors raised by the `loom run` driver.
#[derive(Debug, Display, Error)]
pub enum RunError {
    /// agent backend protocol failure
    Protocol(#[from] ProtocolError),

    /// bd CLI failure
    Bd(#[from] BdError),

    /// rendering the run.md template failed
    Render(#[from] askama::Error),

    /// log sink failure
    Log(#[from] LogError),

    /// git operation failed (worktree, merge, branch)
    Git(#[from] GitError),

    /// io operation failed
    Io(#[from] std::io::Error),

    /// `loom check` handoff failed: {0}
    CheckHandoff(String),
}
