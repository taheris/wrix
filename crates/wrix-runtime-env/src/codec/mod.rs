//! Bounded, redacted guest-only environment delivery preserving empty and multiline values.
use std::{
    ffi::OsString,
    fmt,
    io::{self, Read, Write},
    os::unix::ffi::OsStringExt,
};

pub const GUEST_DIRECTORY: &str = "/run/wrix/runtime-environment";
pub const GUEST_FILE: &str = "/run/wrix/runtime-environment/environment";
pub const GUEST_PROGRAM: &str = "/usr/local/libexec/wrix-runtime-env";
pub const READY_VARIABLE: &str = "WRIX_NETWORK_ENVIRONMENT_READY";
const MAGIC: &[u8] = b"wrix-runtime-env-v1\0";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 131_072;

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid worker environment payload; variable names must be identifiers and values cannot contain NUL
    Invalid,
    /// worker environment payload exceeds its delivery bounds
    TooLarge,
    /// worker environment delivery failed: {0}
    Io(#[from] io::Error),
}

struct Name(String);
impl Name {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let Some((first, tail)) = bytes.split_first() else {
            return Err(Error::Invalid);
        };
        if !(first.is_ascii_alphabetic() || *first == b'_')
            || !tail
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Err(Error::Invalid);
        }
        Ok(Self(
            String::from_utf8(bytes.to_vec()).map_err(|_| Error::Invalid)?,
        ))
    }
}
struct Pair {
    name: Name,
    value: Vec<u8>,
}

pub struct Payload {
    pairs: Vec<Pair>,
}
impl fmt::Debug for Payload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Payload")
            .field("variables", &self.pairs.len())
            .field("contents", &"[REDACTED]")
            .finish()
    }
}
impl Payload {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Result<Self, Error> {
        let mut result = Vec::new();
        let mut size = MAGIC.len();
        for (name, value) in pairs {
            let name = Name::parse(name.as_bytes())?;
            if value.as_bytes().contains(&0) {
                return Err(Error::Invalid);
            }
            size = size
                .checked_add(name.0.len())
                .and_then(|size| size.checked_add(value.len()))
                .and_then(|size| size.checked_add(2))
                .ok_or(Error::TooLarge)?;
            if size > MAX_BYTES || result.len() >= MAX_ENTRIES {
                return Err(Error::TooLarge);
            }
            result.push(Pair {
                name,
                value: value.into_bytes(),
            });
        }
        Ok(Self { pairs: result })
    }
    pub fn write(&self, output: &mut impl Write) -> Result<(), Error> {
        output.write_all(MAGIC)?;
        for pair in &self.pairs {
            output.write_all(pair.name.0.as_bytes())?;
            output.write_all(&[0])?;
            output.write_all(&pair.value)?;
            output.write_all(&[0])?;
        }
        output.flush()?;
        Ok(())
    }
    pub fn read(input: impl Read) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        input
            .take(u64::try_from(MAX_BYTES + 1).map_err(|_| Error::TooLarge)?)
            .read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::TooLarge);
        }
        let bytes = bytes.strip_prefix(MAGIC).ok_or(Error::Invalid)?;
        if bytes.is_empty() {
            return Ok(Self { pairs: Vec::new() });
        }
        let bytes = bytes.strip_suffix(&[0]).ok_or(Error::Invalid)?;
        let mut fields = bytes.split(|byte| *byte == 0);
        let mut pairs = Vec::new();
        while let Some(name) = fields.next() {
            if pairs.len() >= MAX_ENTRIES {
                return Err(Error::TooLarge);
            }
            let value = fields.next().ok_or(Error::Invalid)?;
            pairs.push(Pair {
                name: Name::parse(name)?,
                value: value.to_vec(),
            });
        }
        Ok(Self { pairs })
    }
    pub(crate) fn environment(&self) -> impl Iterator<Item = (&str, OsString)> {
        self.pairs
            .iter()
            .map(|pair| (pair.name.0.as_str(), OsString::from_vec(pair.value.clone())))
    }
}

#[cfg(test)]
mod test;
