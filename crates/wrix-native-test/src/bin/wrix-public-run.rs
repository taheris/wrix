use std::{
    io::{self, Write},
    process::ExitCode,
};
#[path = "public_run/mod.rs"]
mod public_run;
fn main() -> ExitCode {
    match public_run::run(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            match writeln!(
                io::stderr().lock(),
                "public run qualification failed: {error}"
            ) {
                Ok(()) => ExitCode::FAILURE,
                Err(_) => ExitCode::from(74),
            }
        }
    }
}
