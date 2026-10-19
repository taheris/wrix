//! Host-private RSA issuance authority, separate from delegated Nix signing state.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use attic::cache::CacheName;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use displaydoc::Display;
use fs2::FileExt;
use thiserror::Error as ThisError;

const PRIVATE_NAME: &str = "issuer.pem";
const MAX_KEY_BYTES: u16 = 16 * 1024;
const PUBLICATION_LIFETIME_SECONDS: u64 = 3600;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot access host-private Celler issuer state: {0}
    Io(#[from] io::Error),
    /// invalid issuer directory: {0}
    Directory(#[from] wrix_core::private_directory::Error),
    /// issuer authority must be an owner-only regular file in a private directory
    Permissions,
    /// issuer authority and delegated metadata must be separate absolute directories
    Layout,
    /// Celler verification authority does not match the existing host issuer; explicit recovery is required
    VerifierMismatch,
    /// OpenSSL could not generate, parse or sign with Celler issuer authority
    Crypto,
    /// host clock cannot represent the publication credential lifetime
    Clock,
}

/// No private key or credential has a diagnostic or serialization representation.
pub struct Authority {
    root: PathBuf,
}

impl Authority {
    /// Opens existing private authority without generating keys or touching delegated service state.
    pub fn open(root: &Path) -> Result<Self, Error> {
        if !root.is_absolute() {
            return Err(Error::Layout);
        }
        let directory = fs::symlink_metadata(root)?;
        if !directory.is_dir() || directory.mode() & 0o077 != 0 {
            return Err(Error::Permissions);
        }
        public_key(&root.join(PRIVATE_NAME), root)?;
        Ok(Self {
            root: root.to_owned(),
        })
    }

    /// Preserves existing valid authority and installs only its public verifier in delegated metadata.
    /// Callers supply host-controlled paths, never repository or launch configuration.
    /// The issuer directory must also remain outside every other service mount.
    pub fn provision(root: &Path, metadata: &Path) -> Result<Self, Error> {
        if !root.is_absolute() || !metadata.is_absolute() {
            return Err(Error::Layout);
        }
        wrix_core::private_directory::prepare_with_parents(root)?;
        let root = root.canonicalize()?;
        let metadata = metadata.canonicalize()?;
        if root.starts_with(&metadata) || metadata.starts_with(&root) {
            return Err(Error::Layout);
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("issuer.lock"))?;
        check_private(&lock, &root)?;
        lock.lock_exclusive()?;
        let key_path = root.join(PRIVATE_NAME);
        match fs::symlink_metadata(&key_path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let temporary = tempfile::NamedTempFile::new_in(&root)?;
                let generated = Command::new("openssl")
                    .args([
                        "genpkey",
                        "-algorithm",
                        "RSA",
                        "-pkeyopt",
                        "rsa_keygen_bits:2048",
                        "-out",
                    ])
                    .arg(temporary.path())
                    .stdin(Stdio::null())
                    .output()?;
                if !generated.status.success() {
                    return Err(Error::Crypto);
                }
                let public = public_key(temporary.path(), &root)?;
                check_verifier(&metadata, &public)?;
                temporary.as_file().sync_all()?;
                temporary
                    .persist_noclobber(&key_path)
                    .map_err(|error| error.error)?;
                File::open(&root)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        let public = public_key(&key_path, &root)?;
        if !check_verifier(&metadata, &public)? {
            let mut temporary = tempfile::NamedTempFile::new_in(&metadata)?;
            temporary.write_all(&public)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(metadata.join(super::layout::runtime::VERIFIER_NAME))
                .map_err(|error| error.error)?;
            File::open(&metadata)?.sync_all()?;
        }
        Ok(Self { root })
    }

    /// Issues exact-view native management/upload authority for the trusted host publisher.
    /// This is not a sandbox pull credential and must never be delivered to a worker.
    pub fn publication_token(&self, namespace: &CacheName) -> Result<String, Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Clock)?
            .as_secs();
        let expires = now
            .checked_add(PUBLICATION_LIFETIME_SECONDS)
            .ok_or(Error::Clock)?;
        let claims = serde_json::json!({
            "sub": "wrix-host-publisher", "iat": now, "exp": expires,
            "https://jwt.attic.rs/v1": {"caches": {
                (namespace.as_str()): {"r": 1, "w": 1, "cc": 1, "cr": 1, "cq": 1, "cd": 1}
            }}
        });
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let key_path = self.root.join(PRIVATE_NAME);
        let key = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&key_path)?;
        check_private(&key, &self.root)?;
        let mut signer = Command::new("openssl")
            .args(["dgst", "-sha256", "-sign"])
            .arg(&key_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = signer.stdin.take().ok_or(Error::Crypto)?;
        let written = input.write_all(message.as_bytes());
        drop(input);
        let result = signer.wait_with_output()?;
        written?;
        if !result.status.success() {
            return Err(Error::Crypto);
        }
        Ok(format!(
            "{message}.{}",
            URL_SAFE_NO_PAD.encode(result.stdout)
        ))
    }
}

fn check_private(file: &File, root: &Path) -> Result<(), Error> {
    let properties = file.metadata()?;
    let directory = fs::symlink_metadata(root)?;
    if !properties.is_file()
        || properties.mode() & 0o077 != 0
        || properties.nlink() != 1
        || properties.uid() != directory.uid()
        || directory.mode() & 0o077 != 0
    {
        return Err(Error::Permissions);
    }
    Ok(())
}

fn public_key(path: &Path, root: &Path) -> Result<Vec<u8>, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    check_private(&file, root)?;
    if file.metadata()?.len() > u64::from(MAX_KEY_BYTES) {
        return Err(Error::Permissions);
    }
    let output = Command::new("openssl")
        .arg("rsa")
        .arg("-in")
        .arg(path)
        .args(["-check", "-pubout"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.len() > usize::from(MAX_KEY_BYTES) {
        return Err(Error::Crypto);
    }
    // OpenSSL rsa -check writes its success diagnostic before the PEM on stdout.
    let start = output
        .stdout
        .windows(b"-----BEGIN PUBLIC KEY-----".len())
        .position(|bytes| bytes == b"-----BEGIN PUBLIC KEY-----")
        .ok_or(Error::Crypto)?;
    Ok(output.stdout[start..].to_vec())
}

fn check_verifier(metadata: &Path, public: &[u8]) -> Result<bool, Error> {
    let path = metadata.join(super::layout::runtime::VERIFIER_NAME);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::VerifierMismatch);
    }
    let mut existing = Vec::new();
    file.take(u64::from(MAX_KEY_BYTES) + 1)
        .read_to_end(&mut existing)?;
    if existing != public {
        return Err(Error::VerifierMismatch);
    }
    Ok(true)
}

#[cfg(test)]
mod test;
