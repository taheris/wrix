//! SQLite state store backing `.wrapix/loom/state.db`.
//!
//! The schema is owned by `loom-core` and migrated on `StateDb::open`. All
//! raw SQL is confined to this module; callers see a typed Rust surface
//! (`StateDb` plus the row structs returned by its accessors).
//!
//! The state DB is reconstructable from spec files on disk and active beads
//! via [`StateDb::rebuild`]; iteration counters reset to 0 and implementation
//! notes are lost, since notes are written by `loom plan` and have no
//! external source of truth.

mod companions;
mod db;
mod error;
mod rebuild;

pub use companions::parse_companions;
pub use db::{MoleculeRow, SpecRow, StateDb};
pub use error::StateError;
pub use rebuild::{ActiveMolecule, RebuildReport};
