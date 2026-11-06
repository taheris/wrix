//! Token-exact OCI configuration hashing, independent of pretty-printing.

use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid OCI configuration JSON: {0}
    Json(#[from] serde_json::Error),
}

/// A computed SHA-256 identifier of token-exact OCI configuration JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigDigest(sha2::digest::Output<Sha256>);

impl std::fmt::Display for ConfigDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "sha256:{:x}", self.0)
    }
}

/// Hash JSON without insignificant whitespace, preserving every token byte.
/// Re-serialization would change escapes, number spelling and object order.
pub fn config_digest(input: &[u8]) -> Result<ConfigDigest, Error> {
    let _: serde_json::Value = serde_json::from_slice(input)?;
    let mut hasher = Sha256::new();
    let mut quoted = false;
    let mut escaped = false;
    for &byte in input {
        if quoted {
            hasher.update([byte]);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
            hasher.update([byte]);
        } else if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            hasher.update([byte]);
        }
    }
    Ok(ConfigDigest(hasher.finalize()))
}

#[cfg(test)]
mod test;
