use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_core::bd::BdError;
use loom_core::lock::LockError;
use loom_core::state::StateError;

/// Failures raised by [`super::run`] and [`super::fetch_active_molecules`].
#[derive(Debug, Display, Error)]
pub enum InitError {
    /// failed to create directory at {path}
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to write config file at {path}
    WriteConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// lock acquisition failed
    Lock(#[from] LockError),

    /// state-db operation failed
    State(#[from] StateError),

    /// `bd` CLI invocation failed while gathering active molecules
    Bd(#[from] BdError),

    /// active molecule {id} carries no `spec:<label>` label
    MissingSpecLabel { id: String },
}
