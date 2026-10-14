use std::{env, path::PathBuf, process::ExitCode};

use wrix_notify::{
    command,
    navigation::Navigator,
    process::{Host, Runner},
    route::Platform,
};

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
    let interactive = !matches.contains_id("enqueue") && !matches.contains_id("is-focused");
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
            if interactive && platform == Platform::Darwin
                && let Err(error) = Host.run("terminal-notifier", &[
                    "-title", "Wrix navigation failed",
                    "-message", "Enable OmniWM IPC; check Accessibility, app/CLI versions and the command's stderr. Attention/return is retained for retry.",
                ])
            {
                tracing::warn!(%error, "could not display navigation failure notification");
            }
            ExitCode::FAILURE
        }
    }
}
