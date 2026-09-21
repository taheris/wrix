use std::{
    fmt, fs, io,
    path::Path,
    process::{Command, Stdio},
};

use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use displaydoc::Display;
use thiserror::Error;

const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const KEY_ENCODING: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// A Nix trust-key name and a base64-encoded 32-byte Ed25519 public key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachePublicKey(String);

#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// cache public key must have a name followed by a colon and an Ed25519 key
    MissingSeparator,
    /// cache public key name must contain only ASCII letters, digits, dots, underscores, or hyphens
    InvalidName,
    /// invalid cache public key encoding: {source}
    Encoding { source: base64::DecodeError },
    /// cache public key must decode to 32 bytes, got {actual}
    Length { actual: usize },
}

impl CachePublicKey {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let text = input.trim();
        let (name, encoded) = text.split_once(':').ok_or(ParseError::MissingSeparator)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ParseError::InvalidName);
        }
        let decoded = KEY_ENCODING
            .decode(encoded)
            .map_err(|source| ParseError::Encoding { source })?;
        if decoded.len() != ED25519_PUBLIC_KEY_BYTES {
            return Err(ParseError::Length {
                actual: decoded.len(),
            });
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CachePublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub fn ensure_keypair(
    key_name: &str,
    secret_path: &Path,
    public_path: &Path,
    nix_store: &str,
) -> io::Result<()> {
    if secret_path.exists() && read_existing_public_key(public_path)?.is_some() {
        return Ok(());
    }
    generate_keypair(key_name, secret_path, public_path, nix_store)
}

pub fn generate_keypair(
    key_name: &str,
    secret_path: &Path,
    public_path: &Path,
    nix_store: &str,
) -> io::Result<()> {
    let secret_tmp = secret_path.with_extension(format!("secret.{}.tmp", std::process::id()));
    let public_tmp = public_path.with_extension(format!("pub.{}.tmp", std::process::id()));
    remove_if_exists(&secret_tmp)?;
    remove_if_exists(&public_tmp)?;
    let output = Command::new(nix_store)
        .arg("--generate-binary-cache-key")
        .arg(key_name)
        .arg(&secret_tmp)
        .arg(&public_tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        remove_if_exists(&secret_tmp)?;
        remove_if_exists(&public_tmp)?;
        return Err(io::Error::other(format!(
            "failed to generate project cache key with {nix_store}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let public = fs::read_to_string(&public_tmp)?;
    if let Err(error) = CachePublicKey::parse(&public) {
        remove_if_exists(&secret_tmp)?;
        remove_if_exists(&public_tmp)?;
        return Err(io::Error::other(format!(
            "generated project cache public key is invalid: {}: {error}",
            public_path.display()
        )));
    }
    fs::rename(secret_tmp, secret_path)?;
    fs::rename(public_tmp, public_path)
}

fn read_existing_public_key(path: &Path) -> io::Result<Option<CachePublicKey>> {
    match fs::read_to_string(path) {
        Ok(content) => match CachePublicKey::parse(&content) {
            Ok(key) => Ok(Some(key)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "regenerating invalid project cache key");
                Ok(None)
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod test {
    use super::CachePublicKey;

    #[test]
    fn parses_padded_and_unpadded_nix_public_keys() {
        for value in [
            "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            let key = CachePublicKey::parse(&format!(" {value}\n")).unwrap();
            assert_eq!(key.as_str(), value);
            assert_eq!(key.to_string(), value);
        }
    }

    #[test]
    fn rejects_placeholders_malformed_encoding_and_config_injection() {
        for value in [
            "wrix-cache:be619e8138e924f7",
            "wrix-cache-990e10b3394addcf:missing-nix-store-public",
            ":AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "cache key:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "cache\ntrusted-users=root:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "#cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "cache:!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!=",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=:extra",
        ] {
            assert!(CachePublicKey::parse(value).is_err(), "accepted {value:?}");
        }
    }
}
