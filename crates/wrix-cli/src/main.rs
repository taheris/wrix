use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

    if let Err(error) = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(io::stderr)
                .with_target(false)
                .without_time()
                .with_filter(
                    tracing_subscriber::filter::Targets::new()
                        .with_default(tracing::Level::WARN)
                        .with_target("wrix::service::update", tracing::Level::INFO),
                ),
        )
        .try_init()
    {
        let mut stderr = io::stderr().lock();
        if writeln!(stderr, "wrix: failed to initialize logging: {error}").is_err() {
            return ExitCode::FAILURE;
        }
        return ExitCode::FAILURE;
    }

    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr();
    match wrix_cli::command::run_os(&args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "wrix: {error}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}
