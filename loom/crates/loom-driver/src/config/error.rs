use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

/// Errors raised while loading `.wrapix/loom/config.toml`.
#[derive(Debug, Display, Error)]
pub enum LoomConfigError {
    /// failed to read config file at {path}
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// failed to parse loom config
    Parse(#[from] toml::de::Error),
}
