//! Exact immutable image inputs admitted by trusted startup, not worker store administration.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct StorePath(String);

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// invalid immutable image identity
    Identity,
    /// invalid canonical installed-store path
    Path,
}

impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Identity);
        }
        Ok(Self(value))
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}
impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StorePath {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        let tail = value.strip_prefix("/nix/store/").ok_or(Error::Path)?;
        let (hash, name) = tail.split_once('-').ok_or(Error::Path)?;
        if hash.len() != 32
            || !hash
                .bytes()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
            || name.is_empty()
            || name.len() > 211
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
        {
            return Err(Error::Path);
        }
        Ok(Self(value))
    }
}

impl From<StorePath> for String {
    fn from(value: StorePath) -> Self {
        value.0
    }
}
impl StorePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn basename(&self) -> &str {
        &self.0[11..]
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn image_inputs_do_not_select_filesystem_or_retention_destinations() {
        assert!(Id::try_from("a".repeat(64)).is_ok());
        assert!(
            StorePath::try_from("/nix/store/00000000000000000000000000000000-valid".to_owned())
                .is_ok()
        );
        for value in [
            "../escape",
            "/nix/store/00000000000000000000000000000000-a/../../other",
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-unknown",
            "/nix/store/00000000000000000000000000000000-a\n",
        ] {
            assert!(StorePath::try_from(value.to_owned()).is_err());
        }
        for value in [
            "a".repeat(63),
            "g".repeat(64),
            format!("{}\n", "a".repeat(64)),
        ] {
            assert!(Id::try_from(value).is_err());
        }
    }
}
