//! Domain identifier newtypes.
//!
//! Minimal `SpecLabel` and `BeadId` shipped with the GitClient surface. The
//! full identifier set (`MoleculeId`, `ProfileName`, `SessionId`, `ToolCallId`,
//! `RequestId`, plus the `newtype_id!` macro and `serde` derives) lands in
//! wx-3hhwq.2.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecLabel(String);

impl SpecLabel {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpecLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BeadId(String);

impl BeadId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BeadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
