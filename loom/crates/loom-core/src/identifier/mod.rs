//! Domain and protocol identifier newtypes.
//!
//! Each identifier lives in its own submodule so the family layout stays
//! flat and additive (NF-5: nested module structure, no central `types.rs`).
//! All newtypes are produced by the [`newtype_id!`] macro defined here:
//! a `#[serde(transparent)]` tuple struct around `String` with `new()`,
//! `as_str()`, and `Display`.
//!
//! `derive(From)` and `derive(Into)` are deliberately not exposed by the
//! macro (NF-8) — values must enter the newtype through `new()` so that any
//! future parsing logic added to a single id family cannot be bypassed.

mod bead;
mod molecule;
mod profile;
mod request;
mod session;
mod spec;
mod tool_call;

pub use bead::{BeadId, ParseBeadIdError};
pub use molecule::MoleculeId;
pub use profile::ProfileName;
pub use request::RequestId;
pub use session::SessionId;
pub use spec::SpecLabel;
pub use tool_call::ToolCallId;

/// Generate a `#[serde(transparent)]` newtype wrapper around `String`.
///
/// Produces `new(impl Into<String>) -> Self`, `as_str(&self) -> &str`, and
/// `Display`. `From` / `Into` are intentionally NOT derived — see NF-8 in
/// `specs/loom-harness.md`.
macro_rules! newtype_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use newtype_id;
