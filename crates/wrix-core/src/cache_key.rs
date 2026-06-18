use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
};

const ED25519_PUBLIC_KEY_BYTES: usize = 32;

pub fn ensure_keypair(
    key_name: &str,
    secret_path: &Path,
    public_path: &Path,
    nix_store: &str,
) -> io::Result<()> {
    if secret_path.exists() && public_key_file_is_valid(public_path)? {
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
    if !public_key_is_valid(&public) {
        remove_if_exists(&secret_tmp)?;
        remove_if_exists(&public_tmp)?;
        return Err(io::Error::other(format!(
            "generated project cache public key is invalid: {}",
            public_path.display()
        )));
    }
    fs::rename(secret_tmp, secret_path)?;
    fs::rename(public_tmp, public_path)
}

pub fn public_key_is_valid(input: &str) -> bool {
    let trimmed = input.trim();
    let Some((name, encoded)) = trimmed.split_once(':') else {
        return false;
    };
    !name.is_empty()
        && !encoded.is_empty()
        && !encoded.contains(':')
        && base64_decoded_len(encoded) == Some(ED25519_PUBLIC_KEY_BYTES)
}

fn public_key_file_is_valid(path: &Path) -> io::Result<bool> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(public_key_is_valid(&content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn base64_decoded_len(input: &str) -> Option<usize> {
    if input.is_empty() {
        return None;
    }
    let bytes = input.as_bytes();
    let mut padding = 0_usize;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if padding == 0 => {}
            b'=' if index >= bytes.len().saturating_sub(2) => padding += 1,
            _ => return None,
        }
        if padding > 2 {
            return None;
        }
    }
    let remainder = bytes.len() % 4;
    if remainder == 1 || (remainder != 0 && padding != 0) {
        return None;
    }
    let inferred_padding = if remainder == 0 {
        padding
    } else {
        4 - remainder
    };
    Some(((bytes.len() + (4 - remainder) % 4) / 4) * 3 - inferred_padding)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::public_key_is_valid;

    #[test]
    fn accepts_nix_public_key_shape() {
        assert!(public_key_is_valid(
            "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        ));
    }

    #[test]
    fn rejects_short_hash_placeholder() {
        assert!(!public_key_is_valid("wrix-cache:be619e8138e924f7"));
    }

    #[test]
    fn rejects_legacy_missing_placeholder() {
        assert!(!public_key_is_valid(
            "wrix-cache-990e10b3394addcf:missing-nix-store-public"
        ));
    }
}
