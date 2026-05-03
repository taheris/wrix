use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_core::lock::LockError;
use loom_core::state::StateError;

/// Failures raised by [`super::run`] and the helpers it composes.
#[derive(Debug, Display, Error)]
pub enum PlanError {
    /// `loom plan` requires either `-n <label>` or `-u <label>`
    ModeRequired,

    /// `-n <label>` and `-u <label>` are mutually exclusive
    ConflictingModes,

    /// spec file not found at {path} (use `-n <label>` for a new spec)
    SpecMissing { path: PathBuf },

    /// interview exited without writing the spec file at {path}
    InterviewProducedNoSpec { path: PathBuf },

    /// failed to read pinned-context file at {path}
    ReadPinnedContext {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to read spec file at {path}
    ReadSpec {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// askama template render failed
    Render(#[from] askama::Error),

    /// lock acquisition failed
    Lock(#[from] LockError),

    /// state-db operation failed
    State(#[from] StateError),

    /// failed to spawn `wrapix run`
    Spawn {
        #[source]
        source: io::Error,
    },

    /// `wrapix run` exited with status {status}
    WrapixExit { status: String },
}
