pub mod config;
mod launch;

pub use config::{MountMode, ProfileMount, SpawnMount};
pub use launch::{
    DarwinBindMount, DarwinMountPlan, LaunchError, classify_darwin_mounts, run_notification_command,
};

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use config::{Platform, load_profile_config, load_spawn_config};
use launch::{Kind, Request, Run, Spawn};

/// Constructor-owned immutable project launch data; explicit CLI input wins.
pub const PROJECT_PROFILE_CONFIG: &str = "WRIX_PROJECT_PROFILE_CONFIG";

pub const HOST_SETUP: &str = "Provide one host wrix on PATH using Home Manager, nix-darwin, nix profile add 'github:taheris/wrix#wrix', or a temporary environment: nix shell 'github:taheris/wrix#wrix'. No fixed installation directory is required. Projects supply launch data, not another executable. Enter sandbox.devShell for its generated immutable project configuration, or pass --profile-config <file> explicitly. Explicit input overrides the project default for run/spawn only. For incompatible data, update the host package or regenerate supported project data; Wrix does not install or select another CLI automatically.";

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

pub const RUN_HELP: &str = "Run an interactive sandbox.\n\nUsage: wrix [--profile-config <file>] run [LAUNCH_OPTIONS] [DIR] [--] [AGENT_ARGS ...]\n\nDIR defaults to CWD; without DIR, use -- before agent arguments.\nLauncher options stop at DIR or --; after DIR, one optional -- is consumed.\nRemaining arguments are passed unchanged to the agent, including --help.\nOmitted Git options inherit repository policy, defaulting independently to false.\nTool caches: explicit launch choice > repository tool_caches.shared > shared default.\nPrivate tool-cache mode does not disable caching or separate Nix/Celler policy.\nRestricted Rust requires private tool caches; use --no-cache with WRIX_NETWORK=limit.\nInstalled stores: explicit mode > repository nix_store.mode > private default.\nShared installed-store execution is unavailable in this version.\n\nOptions:\n  --profile-config <file>  Override generated project launch data with <file>.\n  --dry-run                Inspect without preparing or executing a launch.\n  --json                   Render the dry-run report as versioned JSON.\n  --nix-store <mode>         Select private or shared installed Nix state.\n  --cache                  Reuse shared worker tool caches for this launch.\n  --no-cache               Use fresh private writable local tool caches for this launch.\n  --git-deploy             Grant the deploy key for this launch; conflicts with --no-git-deploy.\n  --no-git-deploy          Deny the deploy key for this launch; conflicts with --git-deploy.\n  --git-sign               Grant the signing key for this launch; conflicts with --no-git-sign.\n  --no-git-sign            Deny the signing key for this launch; conflicts with --git-sign.\n  -h, --help               Print launcher help before DIR or --.\n";
pub const SPAWN_HELP: &str = "Spawn a programmatic sandbox.\n\nSpawnConfig.tool_caches.shared is an optional boolean: false uses fresh private writable local tool caches; true reuses shared worker tool caches.\nPrecedence: explicit launch choice > repository tool_caches.shared > shared default.\nOmission inherits; this does not disable caching or separate Nix/Celler policy.\nRestricted Rust requires private mode with WRIX_NETWORK=limit.\n\nUsage: wrix [--profile-config <file>] spawn --spawn-config <file> [--stdio]\n\nOptions:\n  --profile-config <file>  Override generated project launch data with <file>.\n  --spawn-config <file>    Read per-launch settings from <file> (required).\n  --dry-run                Inspect without preparing or executing a launch.\n  --json                   Render the dry-run report as versioned JSON.\n  --stdio                  Enable the selected agent's JSONL protocol on standard I/O (default: off).\n  --execution-ticket <id>  Bind a reserved host-only Linux execution ticket.\n  -h, --help               Print help.\n";

pub fn write_run_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(RUN_HELP.as_bytes())?;
    writeln!(stdout, "\n{HOST_SETUP}")
}

pub fn write_spawn_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(SPAWN_HELP.as_bytes())?;
    writeln!(stdout, "\n{HOST_SETUP}")
}

pub fn run(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    run_with_issuer(
        command,
        profile_config_path,
        args,
        stdout,
        stderr,
        &crate::cache_credentials::Unavailable,
    )
}

pub fn run_with_issuer(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    issuer: &dyn crate::cache_credentials::Factory,
) -> io::Result<ExitCode> {
    run_with_factories(
        command,
        profile_config_path,
        args,
        stdout,
        stderr,
        issuer,
        &crate::nix_store::Unavailable,
    )
}

pub fn run_with_factories(
    command: Command,
    profile_config_path: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    issuer: &dyn crate::cache_credentials::Factory,
    stores: &dyn crate::nix_store::Factory,
) -> io::Result<ExitCode> {
    let preview = launch::preview::Options::from_args(command, args);
    match build_request(command, profile_config_path, args) {
        Ok(None) => {
            write_run_help(stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some(request)) => match launch::execute(&request, stdout, stderr, issuer, stores) {
            Ok(code) => Ok(code),
            Err(_error) if preview.dry_run => {
                launch::preview::Report::invalid().write(stdout, preview.json)
            }
            Err(error) => {
                writeln!(stderr, "wrix {}: {error}", command.as_str())?;
                Ok(ExitCode::FAILURE)
            }
        },
        Err(_error) if preview.dry_run => {
            launch::preview::Report::invalid().write(stdout, preview.json)
        }
        Err(error) => {
            writeln!(stderr, "wrix {}: {error}", command.as_str())?;
            if matches!(
                error,
                CliError::MissingProfileConfig | CliError::Config { .. }
            ) {
                writeln!(stderr, "{HOST_SETUP}")?;
            }
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
    let profile_config_path = profile_config_path
        .or_else(|| std::env::var_os(PROJECT_PROFILE_CONFIG).map(PathBuf::from))
        .ok_or(CliError::MissingProfileConfig)?;
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
    let mut preview = launch::preview::Options::default();
    let mut workspace = None;
    let mut git_deploy = None;
    let mut git_sign = None;
    let mut shared = None;
    let mut nix_store = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--dry-run" => preview.dry_run = true,
            "--json" => preview.json = true,
            "--git-deploy" | "--no-git-deploy" => {
                let enabled = arg == "--git-deploy";
                if git_deploy.is_some_and(|value| value != enabled) {
                    return Err(CliError::ConflictingDeployFlags);
                }
                git_deploy = Some(enabled);
            }
            "--nix-store" => {
                index += 1;
                let value = args.get(index).ok_or(CliError::NixStoreValue)?;
                let mode = wrix_core::nix_store::Mode::parse(value)?;
                if nix_store.is_some_and(|previous| previous != mode) {
                    return Err(CliError::ConflictingNixStoreModes);
                }
                nix_store = Some(mode);
            }
            "--cache" | "--no-cache" => {
                let enabled = arg == "--cache";
                if shared.is_some_and(|value| value != enabled) {
                    return Err(CliError::ConflictingCacheFlags);
                }
                shared = Some(enabled);
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
    preview.validate()?;
    Ok(Some(Run {
        preview,
        workspace: workspace.map_or_else(std::env::current_dir, Ok)?,
        agent_args: args[index..].to_vec(),
        git_deploy,
        git_sign,
        tool_caches: wrix_core::tool_cache::Preference { shared },
        nix_store: wrix_core::nix_store::Preference { mode: nix_store },
    }))
}

fn parse_spawn(args: &[String]) -> Result<Spawn, CliError> {
    let mut preview = launch::preview::Options::default();
    let mut spawn_config_path = None;
    let mut execution_ticket = None;
    let mut stdio = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                preview.dry_run = true;
                index += 1;
            }
            "--json" => {
                preview.json = true;
                index += 1;
            }
            "--spawn-config" => {
                let value = args
                    .get(index + 1)
                    .ok_or(CliError::SpawnConfigFlagRequiresValue)?;
                spawn_config_path = Some(PathBuf::from(value));
                index += 2;
            }
            "--execution-ticket" => {
                let value = args
                    .get(index + 1)
                    .ok_or(CliError::SpawnConfigFlagRequiresValue)?;
                execution_ticket = Some(wrix_state::execution::Ticket::parse(value).map_err(
                    |source| CliError::Io {
                        source: io::Error::other(source),
                    },
                )?);
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
    preview.validate()?;
    if execution_ticket.is_none()
        && let Some(value) = std::env::var_os("WRIX_EXECUTION_TICKET")
    {
        let value = value.to_str().ok_or_else(|| CliError::Io {
            source: io::Error::other("invalid host execution ticket encoding"),
        })?;
        execution_ticket = Some(
            wrix_state::execution::Ticket::parse(value).map_err(|source| CliError::Io {
                source: io::Error::other(source),
            })?,
        );
    }
    if execution_ticket.is_some()
        && (Platform::CURRENT != Platform::Linux
            || std::env::var_os("WRIX_UNSAFE_PODMAN_SOCKET").is_some())
    {
        return Err(CliError::Io {
            source: io::Error::other(
                "trusted execution tickets require Linux Podman without host-runtime access",
            ),
        });
    }
    let config_path = spawn_config_path.ok_or(CliError::MissingSpawnConfigFlag)?;
    let config = load_spawn_config(&config_path, Platform::CURRENT)?;
    Ok(Spawn {
        preview,
        config_path,
        config,
        stdio,
        execution_ticket,
    })
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub(super) enum CliError {
    /// --nix-store requires private or shared
    NixStoreValue,
    /// {0}
    NixStoreMode(#[from] wrix_core::nix_store::ModeParseError),
    /// conflicting --nix-store choices
    ConflictingNixStoreModes,
    /// --json requires --dry-run
    JsonRequiresPreview,
    /// wrix requires --profile-config <Nix store ProfileConfig JSON> or generated project launch data from sandbox.devShell
    MissingProfileConfig,
    /// --git-deploy cannot be combined with --no-git-deploy
    ConflictingDeployFlags,
    /// --git-sign cannot be combined with --no-git-sign
    ConflictingSignFlags,
    /// --cache cannot be combined with --no-cache
    ConflictingCacheFlags,
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
    fn run_store_choice_is_strict_and_respects_the_launcher_prefix() {
        let parse = |args: &[&str]| {
            parse_run(
                &args
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        for args in [
            vec!["--nix-store"],
            vec!["--nix-store", "SHARED"],
            vec!["--nix-store", "auto"],
            vec!["--nix-store", "private", "--nix-store", "shared"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        for flag in ["--cache", "--no-cache"] {
            let run = parse(&["--nix-store", "shared", flag]).unwrap().unwrap();
            assert_eq!(run.nix_store.mode, Some(wrix_core::nix_store::Mode::Shared));
            assert_eq!(run.tool_caches.shared, Some(flag == "--cache"));
        }
        for args in [
            vec!["/workspace", "--nix-store", "shared"],
            vec!["--", "--nix-store", "shared"],
        ] {
            let run = parse(&args).unwrap().unwrap();
            assert_eq!(run.nix_store.mode, None);
            assert_eq!(run.agent_args, vec!["--nix-store", "shared"]);
        }
        let repeated = parse(&["--nix-store", "private", "--nix-store", "private"])
            .unwrap()
            .unwrap();
        assert_eq!(
            repeated.nix_store.mode,
            Some(wrix_core::nix_store::Mode::Private)
        );
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
