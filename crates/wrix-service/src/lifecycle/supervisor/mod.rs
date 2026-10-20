//! Planned namespace component transitions without releasing a serving Celler backend.

pub mod client;
pub mod entry;
mod hold;
pub mod serve;
mod state;
pub(crate) mod transition;
pub(crate) mod transport;
mod wire;

use displaydoc::Display;
use std::io;
use thiserror::Error as ThisError;
use wrix_core::path::WorkspaceHash;

pub use wire::{Action, Observation};

pub const DIRECTORY: &str = "/var/lib/wrix/service-supervisor";

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Version {
    FailFast,
    Components,
}

impl Version {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "1" => Some(Self::FailFast),
            "2" => Some(Self::Components),
            _ => None,
        }
    }
}

#[derive(Debug, Display)]
pub enum Role {
    /// Celler
    Celler,
    /// Dolt
    Dolt,
}

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// component supervision I/O failed: {0}
    Io(#[from] io::Error),
    /// component supervision encoding failed: {0}
    Encoding(#[from] serde_json::Error),
    /// {0}
    Directory(#[from] wrix_core::private_directory::Error),
    /// expected a private supervisor mode, directory, namespace and mode-specific arguments
    Arguments,
    /// prior component dispatch is unconfirmed; explicit reconciliation is required before another mutation
    Pending,
    /// component transport worker failed; effects may be unconfirmed
    Worker,
    /// component transport failed with status {code:?}: {diagnostic}
    Transport {
        code: Option<i32>,
        diagnostic: String,
    },
    /// component state must be an owner-only single-link regular file
    Permissions,
    /// component protocol or persistent namespace identity is incompatible
    Incompatible,
    /// prior component ownership ended uncleanly; explicit reconciliation is required
    Unclean,
    /// component intent was recorded, but durability could not be confirmed: {0}
    Committed(io::Error),
    /// component transition is busy; no work was canceled or replayed
    Busy,
    /// component control deadline expired; native work or retirement may still be pending
    Deadline,
    /// component control disconnected; effects may be incomplete
    Disconnected,
    /// component transition remains unconfirmed: {source}
    Unconfirmed { source: Box<Error> },
    /// {role} exited unexpectedly with status {code:?}
    Exited { role: Role, code: Option<i32> },
    /// cannot signal the owned component process; retirement remains unconfirmed
    Signal,
}

pub(super) fn command(namespace: &WorkspaceHash, first: &str, second: &str) -> String {
    format!(
        "exec wrix-service-supervisor serve {} {} {} {} {}",
        quote(DIRECTORY),
        quote(namespace.as_str()),
        quote("/run/wrix/dolt.sock"),
        quote(first),
        quote(second),
    )
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod test;
