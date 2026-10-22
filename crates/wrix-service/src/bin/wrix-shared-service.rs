//! Private service-image entry point.

use std::{path::PathBuf, process::ExitCode};

#[tokio::main]
async fn main() -> ExitCode {
    let result = async {
        let mut args = std::env::args_os().skip(1);
        let data = PathBuf::from(args.next().ok_or(wrix_service::dolt::Error::UnsafePath)?);
        let tcp = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
            Some("unix") => false,
            Some("tcp") => true,
            _ => return Err(wrix_service::dolt::Error::UnsafePath),
        };
        if args.next().is_some() {
            return Err(wrix_service::dolt::Error::UnsafePath);
        }
        wrix_service::dolt::server::run(&data, tcp).await
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            use std::io::Write;
            if let Err(write) = writeln!(std::io::stderr(), "shared service failed: {error}") {
                tracing::error!(%write, "cannot report shared service failure");
            }
            ExitCode::FAILURE
        }
    }
}
