mod config;
mod launch;

pub use config::{MountMode, ProfileMount, SpawnMount};
pub use launch::{DarwinBindMount, DarwinMountPlan, LaunchError, classify_darwin_mounts};

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

pub const RUN_HELP: &str = "Run an interactive sandbox.\n\nUsage: wrix [--profile-config <file>] run [LAUNCH_OPTIONS] [DIR] [--] [AGENT_ARGS ...]\n\nDIR defaults to CWD; without DIR, use -- before agent arguments.\nLauncher options stop at DIR or --; after DIR, one optional -- is consumed.\nRemaining arguments are passed unchanged to the agent, including --help.\nOmitted Git options inherit repository policy, defaulting independently to false.\n\nOptions:\n  --profile-config <file>  Read launcher defaults from <file> before run (required).\n  --git-deploy             Grant the deploy key for this launch; conflicts with --no-git-deploy.\n  --no-git-deploy          Deny the deploy key for this launch; conflicts with --git-deploy.\n  --git-sign               Grant the signing key for this launch; conflicts with --no-git-sign.\n  --no-git-sign            Deny the signing key for this launch; conflicts with --git-sign.\n  -h, --help               Print launcher help before DIR or --.\n";
pub const SPAWN_HELP: &str = "Spawn a programmatic sandbox.\n\nUsage: wrix [--profile-config <file>] spawn --spawn-config <file> [--stdio]\n\nOptions:\n  --profile-config <file>  Read launcher defaults from <file> (required).\n  --spawn-config <file>    Read per-launch settings from <file> (required).\n  --stdio                  Enable the selected agent's JSONL protocol on standard I/O (default: off).\n  -h, --help               Print help.\n";

pub fn write_run_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(RUN_HELP.as_bytes())
}

pub fn write_spawn_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(SPAWN_HELP.as_bytes())
}

pub fn run(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    match build_request(command, profile_config_path, args) {
        Ok(None) => {
            write_run_help(stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some(request)) => match launch::execute(&request, stdout, stderr) {
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

enum Parsed {
    Run(Run),
    Spawn,
}

fn build_request(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
) -> Result<Option<Request>, CliError> {
    let parsed = match command {
        Command::Run => {
            let Some(run) = parse_run(args)? else {
                return Ok(None);
            };
            Parsed::Run(run)
        }
        Command::Spawn => Parsed::Spawn,
    };
    let profile_config_path = profile_config_path.ok_or(CliError::MissingProfileConfig)?;
    let profile_config = load_profile_config(&profile_config_path, Platform::CURRENT)?;
    let kind = match parsed {
        Parsed::Run(run) => Kind::Run(run),
        Parsed::Spawn => Kind::Spawn(parse_spawn(args)?),
    };
    Ok(Some(Request {
        kind,
        profile_config_path,
        profile_config,
    }))
}

/// A missing run request means launcher help was requested in the option prefix.
fn parse_run(args: &[String]) -> Result<Option<Run>, CliError> {
    let mut workspace = None;
    let mut git_deploy = None;
    let mut git_sign = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--git-deploy" | "--no-git-deploy" => {
                let enabled = arg == "--git-deploy";
                if git_deploy.is_some_and(|value| value != enabled) {
                    return Err(CliError::ConflictingDeployFlags);
                }
                git_deploy = Some(enabled);
            }
            "--git-sign" | "--no-git-sign" => {
                let enabled = arg == "--git-sign";
                if git_sign.is_some_and(|value| value != enabled) {
                    return Err(CliError::ConflictingSignFlags);
                }
                git_sign = Some(enabled);
            }
            "--help" | "-h" => return Ok(None),
            "--" => {
                index += 1;
                break;
            }
            option if option.starts_with('-') => {
                return Err(CliError::UnknownRunFlag {
                    flag: option.to_owned(),
                });
            }
            positional => {
                workspace = Some(PathBuf::from(positional));
                index += 1;
                if args.get(index).is_some_and(|arg| arg == "--") {
                    index += 1;
                }
                break;
            }
        }
        index += 1;
    }
    Ok(Some(Run {
        workspace: workspace.map_or_else(std::env::current_dir, Ok)?,
        agent_args: args[index..].to_vec(),
        git_deploy,
        git_sign,
    }))
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
    /// --git-deploy cannot be combined with --no-git-deploy
    ConflictingDeployFlags,
    /// --git-sign cannot be combined with --no-git-sign
    ConflictingSignFlags,
    /// unknown wrix run flag: {flag}; use -- before agent arguments when DIR is omitted
    UnknownRunFlag { flag: String },
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
        let run = parse_run(&args).unwrap().unwrap();
        assert_eq!(run.workspace, std::path::PathBuf::from("/workspace"));
        assert_eq!(run.agent_args, vec![String::from("true")]);
    }

    #[test]
    fn run_parser_preserves_independent_optional_git_overrides() {
        for deploy in [None, Some(false), Some(true)] {
            for sign in [None, Some(false), Some(true)] {
                let mut args = Vec::new();
                if let Some(enabled) = deploy {
                    args.push(String::from(if enabled {
                        "--git-deploy"
                    } else {
                        "--no-git-deploy"
                    }));
                }
                if let Some(enabled) = sign {
                    args.push(String::from(if enabled {
                        "--git-sign"
                    } else {
                        "--no-git-sign"
                    }));
                }
                let run = parse_run(&args).unwrap().unwrap();
                assert_eq!(run.workspace, std::env::current_dir().unwrap());
                assert_eq!(run.agent_args, Vec::<String>::new());
                assert_eq!(run.git_deploy, deploy);
                assert_eq!(run.git_sign, sign);
            }
        }
    }
}
