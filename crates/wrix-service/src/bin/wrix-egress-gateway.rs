//! Private native gateway child inside a managed core-service generation.

use std::{io::Write, process::ExitCode};
use wrix_service::gateway::{Error, bootstrap};

enum Action {
    Serve,
    Ready,
    Status,
    Drain,
}
impl Action {
    fn parse() -> Result<Self, Error> {
        let mut args = std::env::args_os().skip(1);
        let action = match args.next().as_deref() {
            None => Self::Serve,
            Some(arg) if arg == "ready" => Self::Ready,
            Some(arg) if arg == "status" => Self::Status,
            Some(arg) if arg == "drain" => Self::Drain,
            Some(_) => return Err(Error::Invalid),
        };
        if args.next().is_some() {
            return Err(Error::Invalid);
        }
        Ok(action)
    }
}
async fn run() -> Result<(), Error> {
    let action = Action::parse()?;
    let launch = bootstrap::read(tokio::io::stdin()).await?;
    let status = match action {
        Action::Serve => return launch.run().await,
        Action::Ready => launch.client()?.verify(launch.data).await?,
        Action::Status => launch.client()?.status().await?,
        Action::Drain => launch.client()?.drain(true).await?,
    };
    std::io::stdout().write_all(&serde_json::to_vec(&status)?)?;
    Ok(())
}
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Err(write) = writeln!(std::io::stderr(), "native gateway failed: {error}") {
                tracing::error!(%write, "cannot report native gateway failure");
            }
            ExitCode::FAILURE
        }
    }
}
