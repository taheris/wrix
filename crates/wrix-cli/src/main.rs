use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    if let Err(error) = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_max_level(tracing::Level::WARN)
        .with_target(false)
        .without_time()
        .try_init()
    {
        let mut stderr = io::stderr().lock();
        if writeln!(stderr, "wrix: failed to initialize logging: {error}").is_err() {
            return ExitCode::FAILURE;
        }
        return ExitCode::FAILURE;
    }

    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr();
    match wrix_cli::command::run(&args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "wrix: {error}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}
