//! Shared egress policy, private management protocol and scoped secrets; no traffic engine.

pub mod management;
pub mod policy;

use serde::{Deserialize, Serialize};

/// Gateway operation failed: no authorization is implied by an error.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// gateway I/O failed: {0}
    Io(#[from] std::io::Error),
    /// invalid gateway protocol or policy
    Invalid,
    /// gateway authorization denied
    Denied,
    /// gateway destination is protected or outside policy
    Protected,
    /// gateway capacity exhausted
    Capacity,
    /// gateway state lock poisoned
    Poisoned,
    /// gateway protocol encoding failed: {0}
    Json(#[from] serde_json::Error),
}

/// An unguessable scoped secret; Debug never emits its contents.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Secret(String);
impl Secret {
    pub fn generate() -> Result<Self, Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        let mut encoded = String::with_capacity(64);
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
        Ok(Self(encoded))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.0
            .bytes()
            .zip(other.0.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl TryFrom<String> for Secret {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
}
impl From<Secret> for String {
    fn from(value: Secret) -> Self {
        value.0
    }
}
