//! Private supervised host controller entry point; not part of the public Wrix grammar.

use std::{path::PathBuf, process::ExitCode};
use wrix_service::control::Controller;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum Error {
    /// cannot initialize host controller logging: {0}
    Logging(String),
    /// expected exactly one private controller root argument
    Arguments,
    /// private controller I/O failed: {0}
    Io(#[from] std::io::Error),
    /// private controller failed: {0}
    Control(#[from] wrix_service::control::Error),
}

async fn run() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init()
        .map_err(|error| Error::Logging(error.to_string()))?;
    let root = {
        let mut args = std::env::args_os().skip(1);
        let root = PathBuf::from(args.next().ok_or(Error::Arguments)?);
        if args.next().is_some() {
            return Err(Error::Arguments);
        }
        root
    };
    if root.join("stopped").try_exists()? {
        return Ok(());
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let controller = Controller::bind(&root.join("authority"))?;
    loop {
        tokio::select! {
            _ = terminate.recv() => return Ok(controller.close()?),
            request = controller.serve_one() => {
                if let Err(error) = request {
                    tracing::warn!(%error, "private controller request failed");
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "host controller failed");
            ExitCode::FAILURE
        }
    }
}
