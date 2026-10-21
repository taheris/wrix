//! Host-selected external identity domains.

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// integration slot must start with a letter and contain 1–64 ASCII letters, digits, underscores, hyphens or periods
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slot(String);

impl Slot {
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty()
            || value.len() > 64
            || !value.as_bytes()[0].is_ascii_alphabetic()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Slot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
