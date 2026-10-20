//! Private service-image supervisor and namespace component client, not public Wrix grammar.

use std::process::ExitCode;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum Error {
    /// cannot initialize service supervisor logging: {0}
    Logging(String),
    /// service supervision failed: {0}
    Supervision(#[from] wrix_service::lifecycle::supervisor::Error),
}

fn run() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init()
        .map_err(|error| Error::Logging(error.to_string()))?;
    Ok(wrix_service::lifecycle::supervisor::entry::run()?)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "service supervisor failed");
            ExitCode::FAILURE
        }
    }
}
