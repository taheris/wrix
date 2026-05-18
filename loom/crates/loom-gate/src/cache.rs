//! Per-criterion status cache surface.
//!
//! `loom gate` (no subcommand) reads from this sqlite-backed cache and
//! prints a fast report; `loom gate verify` / `loom gate review` and the
//! tier subcommands write to it as they run. Schema and reads/writes live
//! in the cache bead — this scaffold exposes the [`CacheRow`] / [`Verdict`]
//! shape plus a stub [`StatusCache`] type.

use std::path::Path;

use loom_events::identifier::SpecLabel;

use crate::GateError;

/// Per-criterion verdict recorded by the most recent verifier run.
///
/// `Skipped` carries the scope reason (e.g. "annotation outside `--files`
/// set") in the row's `evidence` field; consumers display it alongside the
/// failing rows so a stale skip is visible in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Skipped,
}

/// One row of the status cache. Indexed by `(spec_label, criterion_anchor)`
/// per `specs/loom-gate.md`. `last_run_commit` lets the report distinguish
/// fresh runs from stale runs without re-executing the verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRow {
    pub spec_label: SpecLabel,
    pub criterion_anchor: String,
    pub annotation_target: String,
    pub last_run_ts_ms: i64,
    pub last_run_commit: String,
    pub verdict: Verdict,
    pub evidence: String,
}

/// Handle on the sqlite-backed status cache. Stub — opening, reads, and
/// upserts land in the status-cache bead.
pub struct StatusCache {
    _private: (),
}

impl StatusCache {
    /// Open or create the cache at `path`. Stub — see the status-cache
    /// bead.
    pub fn open(_path: &Path) -> Result<Self, GateError> {
        Err(GateError::Unimplemented {
            what: "cache::StatusCache::open",
        })
    }

    /// Read every row currently persisted. Stub.
    pub fn read_all(&self) -> Result<Vec<CacheRow>, GateError> {
        Err(GateError::Unimplemented {
            what: "cache::StatusCache::read_all",
        })
    }

    /// Insert or update a row keyed by `(spec_label, criterion_anchor)`.
    /// Stub.
    pub fn upsert(&self, _row: &CacheRow) -> Result<(), GateError> {
        Err(GateError::Unimplemented {
            what: "cache::StatusCache::upsert",
        })
    }
}
