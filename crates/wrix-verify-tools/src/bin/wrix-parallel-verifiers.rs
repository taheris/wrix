use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    path::Path,
    process::ExitCode,
};
use wrix_verify_tools::parallel::{self, Error};

fn run(arguments: &[OsString]) -> Result<(), Error> {
    let [directory, program, target, count, parent] = arguments else {
        return Err(Error::Arguments);
    };
    let target = target.to_str().ok_or(Error::Arguments)?;
    let count = count
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::Arguments)?;
    let parent = parent
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::Arguments)?;
    // Outlive cancellation of the caller's process group long enough to stop
    // and reap our job groups. The inherited cache lease remains held meanwhile.
    if rustix::process::getpgrp() != rustix::process::getpid() {
        rustix::process::setpgid(None, None).map_err(io::Error::from)?;
    }
    parallel::run(
        Path::new(directory),
        Path::new(program),
        target,
        count,
        parent,
    )
}

fn main() -> ExitCode {
    match run(&env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if writeln!(io::stderr().lock(), "{error}").is_err() {
                return ExitCode::from(74);
            }
            ExitCode::FAILURE
        }
    }
}
