//! Shared canonicalization + BLAKE3-16 hashing pipeline both observers
//! consume. Per `specs/loom-llm.md` per-result canonicalization happens
//! once; the single utility is invoked from both
//! [`super::doom_loop`] and [`super::duplicate_result`].

use serde_json::Value;

use crate::client::LlmError;

/// 16-byte BLAKE3 hash of a canonical JSON payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultHash(pub [u8; 16]);

/// Shared canonicalization + BLAKE3-16 utility. Both observers consume
/// the same hashing pipeline so per-result canonicalization happens
/// exactly once per `tool_result` event.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResultHasher;

impl ResultHasher {
    /// Construct a fresh hasher. Stateless today; held as a struct so
    /// future canonicalization configuration (e.g. numeric-normalization
    /// thresholds per RFC 8785 JCS) lands here without changing call
    /// sites.
    pub fn new() -> Self {
        Self
    }

    /// Compute the BLAKE3-16 hash of a tool-result `Value`. Returns
    /// `Err(LlmError::Canonicalize)` if canonicalization fails — the
    /// scaffold path always errors so concrete callers wire up RFC 8785
    /// JCS before consuming the hasher.
    pub fn hash(&self, _value: &Value) -> Result<ResultHash, LlmError> {
        Err(LlmError::Canonicalize)
    }
}
