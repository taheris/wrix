use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    path::Path,
    process::{Command, ExitCode},
};
use wrix_verify_tools::retention::{self, Cache};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
enum Error {
    /// usage: wrix-verification-roots init DIRECTORY | plan DIRECTORY MANIFEST | replace DIRECTORY MANIFEST | run DIRECTORY MANIFEST -- COMMAND [ARGS...] | session DIRECTORY -- COMMAND [ARGS...] | session-check DIRECTORY FD PROGRAM | session-clean DIRECTORY | session-frontier DIRECTORY FD PROGRAM NIX NIX-STORE -- [RECIPE...]
    Usage,
    /// {0}
    Retention(#[from] retention::Error),
}

fn run(args: &[OsString]) -> Result<(), Error> {
    match args {
        [mode, directory, separator, program, arguments @ ..]
            if mode == "session" && separator == "--" =>
        {
            retention::session::execute(
                Path::new(directory),
                Command::new(program).args(arguments),
            )?;
        }
        [
            mode,
            directory,
            descriptor,
            program,
            nix,
            nix_store,
            separator,
            recipes @ ..,
        ] if mode == "session-frontier" && separator == "--" => {
            retention::session::frontier::protect(
                Path::new(directory),
                descriptor.to_str().ok_or(Error::Usage)?,
                program,
                Path::new(nix),
                Path::new(nix_store),
                recipes,
            )?;
        }
        [mode, directory, descriptor, program] if mode == "session-check" => {
            retention::session::check(
                Path::new(directory),
                descriptor.to_str().ok_or(Error::Usage)?,
                program,
            )?;
        }
        [mode, directory] if mode == "session-clean" => {
            retention::session::clean(Path::new(directory))?;
        }
        [mode, directory] if mode == "init" => retention::initialize(Path::new(directory))?,
        [mode, directory, manifest] if mode == "plan" || mode == "replace" => {
            let desired = retention::manifest(Path::new(manifest))?;
            let cache = Cache::open(Path::new(directory))?;
            if mode == "plan" {
                cache.plan(&desired, &mut io::stdout().lock())?;
            } else {
                cache.replace(&desired, Path::new("nix-store"), &mut io::stdout().lock())?;
            }
        }
        [
            mode,
            directory,
            manifest,
            separator,
            program,
            arguments @ ..,
        ] if mode == "run" && separator == "--" => {
            let desired = retention::manifest(Path::new(manifest))?;
            let cache = Cache::open(Path::new(directory))?;
            cache.replace(&desired, Path::new("nix-store"), &mut io::stderr().lock())?;
            cache.execute(Command::new(program).args(arguments))?;
        }
        _ => return Err(Error::Usage),
    }
    Ok(())
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
