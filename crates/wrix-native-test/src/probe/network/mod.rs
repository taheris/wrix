pub mod plan;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: network-worker pure|fixed CA PUBLIC-URL PRIVATE-URL SHA256
    Usage,
    /// probe URL does not have the required transport or contains credentials
    Url,
    /// invalid expected SHA-256 content hash
    Hash,
    /// probe worker must run without root identity
    Account,
    /// probe certificate input failed
    Certificate,
    /// probe network request failed
    Request,
    /// ordinary derivation unexpectedly gained network access
    Ordinary,
    /// fixed-output derivation payload does not match its expected hash
    Content,
    /// fixed-output derivation reached the private service
    Private,
    /// probe output must be an absolute path
    Output,
    /// probe I/O failed: {0}
    Io(#[from] io::Error),
}
#[derive(Clone, Copy)]
pub enum Mode {
    Pure,
    Fixed,
}
pub struct Public(reqwest::Url);
pub struct Private(reqwest::Url);
pub struct Hash(String);
impl Public {
    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(Self(url(value, Transport::Tls)?))
    }
}
impl Private {
    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(Self(url(value, Transport::Http)?))
    }
}
#[derive(Clone, Copy)]
enum Transport {
    Tls,
    Http,
}
fn url(value: &str, transport: Transport) -> Result<reqwest::Url, Error> {
    let scheme = match transport {
        Transport::Tls => "https",
        Transport::Http => "http",
    };
    let url = reqwest::Url::parse(value).map_err(|_| Error::Url)?;
    if url.scheme() != scheme
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Url);
    }
    Ok(url)
}
impl Hash {
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
        {
            return Err(Error::Hash);
        }
        Ok(Self(value.into()))
    }
    fn matches(&self, bytes: &[u8]) -> bool {
        format!("{:x}", Sha256::digest(bytes)) == self.0
    }
}
fn client(ca: &Path) -> Result<reqwest::Client, Error> {
    let mut certificates = Vec::new();
    File::open(ca)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut certificates)?;
    if certificates.len() > 4 * 1024 * 1024 {
        return Err(Error::Certificate);
    }
    let certificates =
        reqwest::Certificate::from_pem_bundle(&certificates).map_err(|_| Error::Certificate)?;
    if certificates.is_empty() {
        return Err(Error::Certificate);
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false);
    for certificate in certificates {
        builder = builder.add_root_certificate(certificate);
    }
    builder.build().map_err(|_| Error::Certificate)
}
async fn fetch(
    client: &reqwest::Client,
    url: &reqwest::Url,
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    let mut response = client
        .get(url.clone())
        .timeout(timeout)
        .send()
        .await
        .map_err(|_| Error::Request)?
        .error_for_status()
        .map_err(|_| Error::Request)?;
    let mut bytes = Vec::new();
    while bytes.len() < 4096 {
        let Some(chunk) = response.chunk().await.map_err(|_| Error::Request)? else {
            break;
        };
        let count = chunk.len().min(4096 - bytes.len());
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}
async fn observe(
    mode: Mode,
    client: &reqwest::Client,
    public: &Public,
    private: &Private,
    expected: &Hash,
) -> Result<Vec<u8>, Error> {
    match mode {
        Mode::Pure => match fetch(client, &public.0, Duration::from_secs(2)).await {
            Ok(_) => Err(Error::Ordinary),
            Err(Error::Request) => Ok(b"network-isolated".to_vec()),
            Err(error) => Err(error),
        },
        Mode::Fixed => {
            let body = fetch(client, &public.0, Duration::from_secs(20)).await?;
            if !expected.matches(&body) {
                return Err(Error::Content);
            }
            match fetch(client, &private.0, Duration::from_secs(2)).await {
                Ok(_) => Err(Error::Private),
                Err(Error::Request) => Ok(body),
                Err(error) => Err(error),
            }
        }
    }
}
pub fn run(args: &[OsString]) -> Result<(), Error> {
    let [mode, ca, public, private, expected] = args else {
        return Err(Error::Usage);
    };
    let mode = match mode.to_str() {
        Some("pure") => Mode::Pure,
        Some("fixed") => Mode::Fixed,
        _ => return Err(Error::Usage),
    };
    let public = Public::parse(public.to_str().ok_or(Error::Url)?)?;
    let private = Private::parse(private.to_str().ok_or(Error::Url)?)?;
    let expected = Hash::parse(expected.to_str().ok_or(Error::Hash)?)?;
    if rustix::process::getuid().is_root() {
        return Err(Error::Account);
    }
    let out = std::env::var_os("out").ok_or(Error::Output)?;
    let out = Path::new(&out);
    if !out.is_absolute() {
        return Err(Error::Output);
    }
    let client = client(Path::new(ca))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let bytes = runtime.block_on(observe(mode, &client, &public, &private, &expected))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod tls_test;
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn sha256_expected_value_is_exact_lowercase_content_identity() {
        let expected =
            Hash::parse("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        assert!(expected.matches(b""));
        assert!(!expected.matches(b"other"));
        for data in ["a".repeat(63), "A".repeat(64), "g".repeat(64)] {
            assert!(Hash::parse(&data).is_err());
        }
    }
    #[test]
    fn public_and_private_transport_contracts_are_distinct() {
        assert!(Public::parse("https://cache.nixos.org/nix-cache-info").is_ok());
        assert!(Private::parse("http://10.231.47.1:24823").is_ok());
        assert!(Public::parse("http://cache.nixos.org/").is_err());
        assert!(Private::parse("https://10.231.47.1/").is_err());
    }
    #[test]
    fn credentials_and_fragments_never_enter_a_network_probe() {
        for value in [
            "https://user:secret@example.invalid/",
            "https://example.invalid/#fragment",
            "not a URL",
        ] {
            assert!(Public::parse(value).is_err());
        }
    }
    #[test]
    fn malformed_ca_bundle_cannot_make_an_insecure_client() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not a certificate").unwrap();
        assert!(client(file.path()).is_err());
    }
}
