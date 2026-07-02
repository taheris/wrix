use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut stderr = io::stderr().lock();
    match wrix_cli::sign::run_program(args) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "wrix git sign: {error}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}
