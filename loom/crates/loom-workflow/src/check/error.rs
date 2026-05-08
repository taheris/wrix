use displaydoc::Display;
use thiserror::Error;

use loom_core::agent::ProtocolError;
use loom_core::bd::BdError;
use loom_core::logging::LogError;
use loom_core::profile_manifest::ProfileError;
use loom_core::state::StateError;

use crate::spec::SpecError;

/// Errors raised by the `loom check` driver.
#[derive(Debug, Display, Error)]
pub enum CheckError {
    /// agent backend protocol failure
    Protocol(#[from] ProtocolError),

    /// bd CLI failure
    Bd(#[from] BdError),

    /// rendering the check.md template failed
    Render(#[from] askama::Error),

    /// log sink failure
    Log(#[from] LogError),

    /// io operation failed
    Io(#[from] std::io::Error),

    /// reviewer agent did not emit LOOM_COMPLETE: {0}
    ReviewIncomplete(String),

    /// `git push` failed: {0}
    GitPushFailed(String),

    /// `beads-push` failed after `git push` succeeded: {0}
    BeadsPushFailed(String),

    /// detached HEAD — refuse to push
    DetachedHead,

    /// `loom run` handoff for auto-iteration failed: {0}
    RunHandoff(String),

    /// state-db read/write failure
    State(#[from] StateError),

    /// profile-image manifest dispatch failed
    Profile(#[from] ProfileError),

    /// no active molecule for spec {0} — run `loom todo` before `loom check`
    NoActiveMolecule(String),

    /// failed to load `[verify]`/`[judge]` sources for review prompt
    Spec(#[from] SpecError),
}
