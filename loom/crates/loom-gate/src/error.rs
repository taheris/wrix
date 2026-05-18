use displaydoc::Display;
use thiserror::Error;

/// Errors surfaced by the `loom-gate` runtime.
///
/// `Unimplemented` is the scaffold's stand-in for unfinished surfaces —
/// every module exports stub entry points that return this variant until
/// its implementation bead lands. Per RS-9, stubs never panic via
/// `todo!()` / `unimplemented!()`; an error variant is the production path.
#[derive(Debug, Display, Error)]
pub enum GateError {
    /// not yet implemented: {what}
    Unimplemented { what: &'static str },
}
