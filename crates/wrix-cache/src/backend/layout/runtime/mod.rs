//! Verification-only configuration for the packaged service backend.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use displaydoc::Display;
use thiserror::Error as ThisError;

/// Retains exclusive metadata ownership in the backend process; contention exits 75 without opening Celler.
pub const COMMAND: &str = "exec flock --exclusive --nonblock --conflict-exit-code 75 --no-fork /var/lib/wrix/celler/backend.lock cellerd --config /var/lib/wrix/celler/server.toml";
pub const CONFIG_NAME: &str = "server.toml";
pub const VERIFIER_NAME: &str = "jwt.pub";

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot install Celler runtime configuration: {0}
    Io(#[from] io::Error),
    /// provision the host controller's Celler RSA public verification key before starting the backend
    Verifier,
}

/// Installs non-secret runtime settings, never issuer or publisher authority.
/// Requires separately provisioned public verification authority; Celler parses the key.
pub fn install(metadata: &Path) -> Result<(), Error> {
    let verifier = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(metadata.join(VERIFIER_NAME))
        .map_err(|_| Error::Verifier)?;
    let properties = verifier.metadata()?;
    if !properties.is_file() || properties.len() == 0 || properties.len() > 16 * 1024 {
        return Err(Error::Verifier);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(metadata)?;
    temporary.write_all(configuration().as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(metadata.join(CONFIG_NAME))
        .map_err(|error| error.error)?;
    fs::File::open(metadata)?.sync_all()?;
    Ok(())
}

const fn configuration() -> &'static str {
    "listen = \"0.0.0.0:8080\"\n\
soft-delete-caches = false\n\
require-proof-of-possession = true\n\
[database]\n\
url = \"sqlite:///var/lib/wrix/celler/metadata.db?mode=rwc\"\n\
[storage]\n\
type = \"local\"\n\
path = \"/var/cache/wrix/celler\"\n\
[chunking]\n\
nar-size-threshold = 0\n\
min-size = 16384\n\
avg-size = 65536\n\
max-size = 262144\n\
[compression]\n\
type = \"none\"\n\
[garbage-collection]\n\
interval = \"12h\"\n\
[jwt]\n\
rs256-public-key-file = \"/var/lib/wrix/celler/jwt.pub\"\n"
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn installation_preserves_verification_key_and_native_data() {
        let root = tempfile::tempdir().unwrap();
        for name in [VERIFIER_NAME, "metadata.db"] {
            fs::write(root.path().join(name), name).unwrap();
        }
        install(root.path()).unwrap();
        let config: toml::Value =
            toml::from_str(&fs::read_to_string(root.path().join(CONFIG_NAME)).unwrap()).unwrap();
        assert_eq!(config["jwt"].as_table().unwrap().len(), 1);
        assert_eq!(
            config["jwt"]["rs256-public-key-file"].as_str(),
            Some("/var/lib/wrix/celler/jwt.pub")
        );
        assert_eq!(config["soft-delete-caches"].as_bool(), Some(false));
        assert_eq!(config["require-proof-of-possession"].as_bool(), Some(true));
        for name in [VERIFIER_NAME, "metadata.db"] {
            assert_eq!(fs::read_to_string(root.path().join(name)).unwrap(), name);
        }
        install(root.path()).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join(CONFIG_NAME)).unwrap(),
            configuration()
        );
    }
}
