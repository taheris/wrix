use std::fmt;

use displaydoc::Display;
use thiserror::Error;

/// A trimmed, non-empty Git remote name without embedded whitespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Name(String);

impl Name {
    pub fn parse(value: &str) -> Result<Self, ParseError> {
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(ParseError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Display, Eq, Error, PartialEq)]
pub enum ParseError {
    /// Git remote name must be non-empty and contain no whitespace: {value}
    Invalid { value: String },
}
