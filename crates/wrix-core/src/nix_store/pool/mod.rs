//! Identities describe installed pools; none is an admission capability.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
/// repository identity must contain exactly 32 lowercase hexadecimal characters
pub struct ParseError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Repository(String);

impl TryFrom<String> for Repository {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ParseError);
        }
        Ok(Self(value))
    }
}

impl From<Repository> for String {
    fn from(value: Repository) -> Self {
        value.0
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum System {
    #[serde(rename = "x86_64-linux")]
    X86_64Linux,
    #[serde(rename = "aarch64-linux")]
    Aarch64Linux,
}

impl System {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::Aarch64Linux => "aarch64-linux",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub repository: Repository,
    pub system: System,
}
