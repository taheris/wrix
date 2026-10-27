//! Private service-image entry point.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};
use wrix_service::{
    dolt::Error,
    lifecycle::{
        compiler,
        drain::{Command, Role},
    },
};

enum Entry {
    Gateway,
    PlannedStop,
    CompilerReleased,
    CompilerReady,
    Sql(Command),
    Backend(Role, Command),
    Run { data: PathBuf, tcp: bool },
}

impl Entry {
    fn parse(arguments: &[OsString]) -> Result<Self, Error> {
        let first = arguments.first().ok_or(Error::UnsafePath)?;
        let tail = &arguments[1..];
        match first.to_str() {
            Some("gateway") if tail.is_empty() => Ok(Self::Gateway),
            Some("backend-planned-stop") if tail.is_empty() => Ok(Self::PlannedStop),
            Some("compiler-released") if tail.is_empty() => Ok(Self::CompilerReleased),
            Some("compiler-ready") if tail.is_empty() => Ok(Self::CompilerReady),
            Some("sql-drain") => match tail {
                [action] => {
                    let command = action_command(action)?;
                    if command == Command::Release {
                        return Err(Error::UnsafePath);
                    }
                    Ok(Self::Sql(command))
                }
                _ => Err(Error::UnsafePath),
            },
            Some("backend-drain") => match tail {
                [role, action] => {
                    let role = match role.to_str() {
                        Some("celler") => Role::Celler,
                        Some("compiler") => Role::Compiler,
                        _ => return Err(Error::UnsafePath),
                    };
                    Ok(Self::Backend(role, action_command(action)?))
                }
                _ => Err(Error::UnsafePath),
            },
            Some("gateway" | "backend-planned-stop" | "compiler-released" | "compiler-ready") => {
                Err(Error::UnsafePath)
            }
            _ => match tail {
                [mode] => {
                    let tcp = match mode.to_str() {
                        Some("unix") => false,
                        Some("tcp") => true,
                        _ => return Err(Error::UnsafePath),
                    };
                    Ok(Self::Run {
                        data: PathBuf::from(first),
                        tcp,
                    })
                }
                _ => Err(Error::UnsafePath),
            },
        }
    }

    async fn run(self) -> Result<(), Error> {
        match self {
            Self::Gateway => wrix_service::lifecycle::generation::serve_gateway()
                .await
                .map_err(|error| Error::Io(std::io::Error::other(error))),
            Self::PlannedStop => compiler::plan_stop(),
            Self::CompilerReleased => {
                if compiler::released()? {
                    Ok(())
                } else {
                    Err(Error::Incomplete)
                }
            }
            Self::CompilerReady => compiler::ready(),
            Self::Sql(command) => response(
                &wrix_service::lifecycle::sql::dispatch(command).map_err(std::io::Error::other)?,
            ),
            Self::Backend(role, command) => response(
                &wrix_service::lifecycle::drain::dispatch(role, command)
                    .map_err(std::io::Error::other)?,
            ),
            Self::Run { data, tcp } => compiler::run(&data, tcp).await,
        }
    }
}

fn action_command(action: &std::ffi::OsStr) -> Result<Command, Error> {
    match action.to_str() {
        Some("begin") => Ok(Command::Begin),
        Some("quiesce") => Ok(Command::Quiesce),
        Some("status") => Ok(Command::Status),
        Some("abort") => Ok(Command::Abort),
        Some("release") => Ok(Command::Release),
        _ => Err(Error::UnsafePath),
    }
}

fn response(response: &wrix_service::lifecycle::drain::Response) -> Result<(), Error> {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, response)?;
    output.write_all(b"\n")?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Entry::parse(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(entry) => entry.run().await,
        Err(error) => Err(error),
    };
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

#[cfg(test)]
mod test {
    use super::*;

    fn parse(args: &[&str]) -> Result<Entry, Error> {
        Entry::parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn private_commands_reject_extra_arguments_before_effects() {
        for command in [
            "gateway",
            "backend-planned-stop",
            "compiler-released",
            "compiler-ready",
        ] {
            assert!(parse(&[command]).is_ok());
            assert!(parse(&[command, "tcp"]).is_err());
        }
        assert!(parse(&["sql-drain", "begin", "extra"]).is_err());
        assert!(parse(&["backend-drain", "celler", "release", "extra"]).is_err());
    }

    #[test]
    fn native_drain_commands_have_fixed_roles_and_actions() {
        for action in ["begin", "status", "quiesce", "abort"] {
            assert!(matches!(
                parse(&["sql-drain", action]).unwrap(),
                Entry::Sql(_)
            ));
            for role in ["celler", "compiler"] {
                assert!(matches!(
                    parse(&["backend-drain", role, action]).unwrap(),
                    Entry::Backend(_, _)
                ));
            }
        }
        assert!(matches!(
            parse(&["backend-drain", "compiler", "release"]).unwrap(),
            Entry::Backend(Role::Compiler, Command::Release)
        ));
        for args in [
            vec!["sql-drain", "release"],
            vec!["sql-drain", "/tmp/socket"],
            vec!["backend-drain", "dolt", "status"],
            vec!["backend-drain", "celler", "unknown"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn normal_backend_entry_retains_compiler_supervision_and_transport() {
        assert!(matches!(
            parse(&["/data/dolt", "unix"]).unwrap(),
            Entry::Run { tcp: false, .. }
        ));
        assert!(matches!(
            parse(&["/data/dolt", "tcp"]).unwrap(),
            Entry::Run { tcp: true, .. }
        ));
        assert!(parse(&["/data/dolt", "tcp", "extra"]).is_err());
    }
}
