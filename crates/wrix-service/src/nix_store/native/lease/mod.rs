//! One native launch identity and its SSH authentication key.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// invalid installed-store launch identity
    Identity,
    /// invalid installed-store Ed25519 public key
    Key,
}

impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 32
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Key(String);

impl TryFrom<String> for Key {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        let encoded = value.strip_prefix("ssh-ed25519 ").ok_or(Error::Key)?;
        if encoded.len() != 68 {
            return Err(Error::Key);
        }
        let bytes = STANDARD.decode(encoded).map_err(|_| Error::Key)?;
        if bytes.len() != 51
            || !bytes.starts_with(b"\0\0\0\x0bssh-ed25519\0\0\0\x20")
            || STANDARD.encode(&bytes) != encoded
        {
            return Err(Error::Key);
        }
        Ok(Self(value))
    }
}

impl From<Key> for String {
    fn from(value: Key) -> Self {
        value.0
    }
}

impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn identities_cannot_select_paths_or_command_fragments() {
        assert!(Id::try_from("0123456789abcdef0123456789abcdef".to_owned()).is_ok());
        for value in [
            "",
            "../escape",
            "0123456789abcdef0123456789abcdeF",
            "00000000000000000000000000000000\n",
        ] {
            assert!(Id::try_from(value.to_owned()).is_err());
        }
    }

    #[test]
    fn authentication_keys_reject_options_comments_and_noncanonical_wire() {
        let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        wire.extend([7_u8; 32]);
        let value = format!("ssh-ed25519 {}", STANDARD.encode(&wire));
        assert!(Key::try_from(value.clone()).is_ok());
        for value in [
            format!("command=evil {value}"),
            format!("{value} comment"),
            format!("{value}\n"),
            "ssh-ed25519 AAAA".to_owned(),
        ] {
            assert!(Key::try_from(value).is_err());
        }
    }
}
