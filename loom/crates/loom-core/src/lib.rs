//! Shared types and infrastructure for the Loom agent driver.
//!
//! Holds newtype identifiers, the agent backend trait surface, the SQLite
//! state store, the typed `bd` CLI wrapper, and per-spec advisory locking.
//! Subsequent issues populate each module; this crate currently exposes the
//! module skeleton only.

pub mod agent;
pub mod bd;
pub mod git;
pub mod identifier;
pub mod lock;
pub mod logging;
pub mod state;
