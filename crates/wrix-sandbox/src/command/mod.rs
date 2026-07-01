mod config;
mod launch;

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use config::{Platform, load_profile_config, load_spawn_config};
use launch::{Kind, Request, Run, Spawn};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Run,
    Spawn,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "run" => Some(Self::Run),
            "spawn" => Some(Self::Spawn),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Spawn => "spawn",
        }
    }
}

pub fn write_run_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(b"Run an interactive sandbox.\n\nUsage: wrix [--profile-config <file>] run [DIR] [CMD ...]\n")
}

pub fn write_spawn_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Spawn a programmatic sandbox.\n\nUsage: wrix [--profile-config <file>] spawn --spawn-config <file> [--stdio]\n",
    )
}

pub fn run(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    match build_request(command, profile_config_path, args) {
        Ok(request) => match launch::execute(&request, stdout) {
            Ok(code) => Ok(code),
            Err(error) => {
                writeln!(stderr, "wrix {}: {error}", command.as_str())?;
                Ok(ExitCode::FAILURE)
            }
        },
        Err(error) => {
            writeln!(stderr, "wrix {}: {error}", command.as_str())?;
            Ok(ExitCode::FAILURE)
        }
    }
}

fn build_request(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
) -> Result<Request, CliError> {
    let profile_config_path = profile_config_path.ok_or(CliError::MissingProfileConfig)?;
    let profile_config = load_profile_config(&profile_config_path, Platform::CURRENT)?;
    let kind = match command {
        Command::Run => Kind::Run(parse_run(args)?),
        Command::Spawn => Kind::Spawn(parse_spawn(args)?),
    };
    Ok(Request {
        kind,
        profile_config_path,
        profile_config,
    })
}

fn parse_run(args: &[String]) -> Result<Run, CliError> {
    let workspace = args
        .first()
        .map(PathBuf::from)
        .map_or_else(std::env::current_dir, Ok)?;
    let agent_args = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };
    Ok(Run {
        workspace,
        agent_args,
    })
}

fn parse_spawn(args: &[String]) -> Result<Spawn, CliError> {
    let mut spawn_config_path = None;
    let mut stdio = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--spawn-config" => {
                let value = args
                    .get(index + 1)
                    .ok_or(CliError::SpawnConfigFlagRequiresValue)?;
                spawn_config_path = Some(PathBuf::from(value));
                index += 2;
            }
            "--stdio" => {
                stdio = true;
                index += 1;
            }
            "--" => break,
            other => {
                return Err(CliError::UnknownSpawnFlag {
                    flag: other.to_owned(),
                });
            }
        }
    }
    let config_path = spawn_config_path.ok_or(CliError::MissingSpawnConfigFlag)?;
    let config = load_spawn_config(&config_path, Platform::CURRENT)?;
    Ok(Spawn {
        config_path,
        config,
        stdio,
    })
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum CliError {
    /// wrix requires --profile-config <Nix store ProfileConfig JSON>
    MissingProfileConfig,
    /// --spawn-config requires <file>
    SpawnConfigFlagRequiresValue,
    /// wrix spawn requires --spawn-config <file>
    MissingSpawnConfigFlag,
    /// unknown wrix spawn flag: {flag}
    UnknownSpawnFlag { flag: String },
    /// {source}
    Config { source: config::ConfigError },
    /// {source}
    Io { source: io::Error },
}

impl From<config::ConfigError> for CliError {
    fn from(source: config::ConfigError) -> Self {
        Self::Config { source }
    }
}

impl From<io::Error> for CliError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

#[cfg(test)]
mod test {
    use super::{Command, parse_run};

    #[test]
    fn command_parser_accepts_launcher_subcommands() {
        assert_eq!(Command::parse("run"), Some(Command::Run));
        assert_eq!(Command::parse("spawn"), Some(Command::Spawn));
        assert_eq!(Command::parse("service"), None);
    }

    #[test]
    fn run_parser_treats_first_positional_as_workspace() {
        let args = vec![String::from("/workspace"), String::from("true")];
        let run = parse_run(&args).unwrap();
        assert_eq!(run.workspace, std::path::PathBuf::from("/workspace"));
        assert_eq!(run.agent_args, vec![String::from("true")]);
    }
}
