use std::{env, path::PathBuf, process::ExitCode};

use wrix_notify::{command, navigation::Navigator, process::Host, route::Platform};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .with_target(false)
        .with_ansi(false)
        .init();
    let matches = command::parser().get_matches();
    let platform = Platform::host();
    let result = command::mode(&matches).and_then(|mode| {
        let directory = command::session_directory(platform, |name| env::var_os(name))?;
        command::run(
            mode,
            &directory,
            platform,
            &Navigator::new(Host, PathBuf::from("/proc")),
        )
    });
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            tracing::error!(%error, command = "wrix-focus-notification", "attention command failed");
            ExitCode::FAILURE
        }
    }
}
