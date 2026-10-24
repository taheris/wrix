use std::{io, process::ExitCode};
use tokio::io::AsyncWriteExt;
use wrix_service::nix_store::native::{self, Role, lease};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(status) => status,
        Err(error) => {
            tracing::error!(%error, "installed-store guest operation failed");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode, native::Error> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "serve" => native::serve().await?,
        [command] if command == "control" => native::control().await?,
        [command] if command == "resume" => native::resume()?,
        [command] if command == "seal" => native::seal()?,
        [command, image] if command == "seed-import" => {
            native::seed_import(&native::image::Id::try_from(image.clone())?)?;
        }
        [command] if command == "storage" => {
            let storage = native::storage()?;
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(format!("{}\n", storage.as_str()).as_bytes())
                .await?;
            stdout.flush().await?;
        }
        [command, user] if command == "keys" => {
            let mut stdout = tokio::io::stdout();
            for key in native::keys(user)? {
                stdout.write_all(format!("{key}\n").as_bytes()).await?;
            }
            stdout.flush().await?;
        }
        [command, id] if command == "client" || command == "export" => {
            let role = if command == "client" {
                Role::Client
            } else {
                Role::Export
            };
            return native::session(role, lease::Id::try_from(id.clone())?).await;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid installed-store guest operation",
            )
            .into());
        }
    }
    Ok(ExitCode::SUCCESS)
}
