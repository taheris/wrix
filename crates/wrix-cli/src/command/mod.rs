use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Arg, ArgAction, ArgMatches, Command as ClapCommand, error::ErrorKind};

const HELP_FLAG: &str = "help-flag";
const PASSTHROUGH_ARGS: &str = "args";
const PROFILE_CONFIG: &str = "profile-config";

pub fn run(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_os(&args, stdout, stderr)
}

pub fn run_os(
    args: &[OsString],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    let root = match parse_root(args) {
        Ok(root) => root,
        Err(error) => return render_clap_error(&error, stdout, stderr),
    };
    let RootInvocation {
        profile_config,
        invocation,
    } = root;

    match invocation {
        Some(Invocation::Run(args)) => run_sandbox(
            wrix_sandbox::command::Command::Run,
            profile_config,
            &args,
            stdout,
            stderr,
        ),
        Some(Invocation::Spawn(args)) => run_sandbox(
            wrix_sandbox::command::Command::Spawn,
            profile_config,
            &args,
            stdout,
            stderr,
        ),
        Some(Invocation::Notify(args)) => run_notify(&args, stdout, stderr),
        Some(Invocation::Service(args)) => run_service(&args, stdout, stderr),
        Some(Invocation::Beads(args)) => run_beads(&args, stdout, stderr),
        Some(Invocation::Workspace(args)) => crate::workspace::run(&args, stdout, stderr),
        Some(Invocation::Init(args)) => {
            crate::init::run(profile_config.as_deref(), &args, stdout, stderr)
        }
        Some(Invocation::Help(args)) => write_delegated_help(&args, stdout, stderr),
        None => {
            write_help(stdout)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

struct RootInvocation {
    profile_config: Option<PathBuf>,
    invocation: Option<Invocation>,
}

enum Invocation {
    Run(Vec<String>),
    Spawn(Vec<String>),
    Notify(Vec<String>),
    Service(Vec<String>),
    Beads(Vec<String>),
    Workspace(Vec<OsString>),
    Init(Vec<String>),
    Help(Vec<String>),
}

fn parse_root(args: &[OsString]) -> Result<RootInvocation, clap::Error> {
    let (root_args, run_args) = split_run_arguments(args);
    let matches = root_command().try_get_matches_from(
        std::iter::once(OsStr::new("wrix")).chain(root_args.iter().map(OsString::as_os_str)),
    )?;
    let profile_config = matches.get_one::<PathBuf>(PROFILE_CONFIG).cloned();
    let invocation = match matches.subcommand() {
        Some(("run", _matches)) => Some(Invocation::Run(utf8_arguments(run_args)?)),
        Some(("spawn", matches)) => Some(Invocation::Spawn(utf8_arguments(&passthrough_values(
            matches,
        ))?)),
        Some(("notify", matches)) => Some(Invocation::Notify(utf8_arguments(
            &passthrough_values(matches),
        )?)),
        Some(("service", matches)) => Some(Invocation::Service(utf8_arguments(
            &passthrough_values(matches),
        )?)),
        Some(("beads", matches)) => Some(Invocation::Beads(utf8_arguments(&passthrough_values(
            matches,
        ))?)),
        Some(("init", matches)) => Some(Invocation::Init(utf8_arguments(&passthrough_values(
            matches,
        ))?)),
        Some(("workspace", matches)) => Some(Invocation::Workspace(passthrough_values(matches))),
        Some(("help", matches)) => Some(Invocation::Help(utf8_arguments(&passthrough_values(
            matches,
        ))?)),
        Some(_) | None => None,
    };
    if profile_config.is_some()
        && !matches!(invocation, Some(Invocation::Run(_) | Invocation::Spawn(_)))
    {
        return Err(root_command().error(
            ErrorKind::ArgumentConflict,
            "--profile-config is only valid for run and spawn",
        ));
    }
    Ok(RootInvocation {
        profile_config,
        invocation,
    })
}

/// Root parsing ends at `run`; its entire tail belongs to the sandbox parser.
fn split_run_arguments(args: &[OsString]) -> (&[OsString], &[OsString]) {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.to_str() {
            Some("--profile-config") => index += 2,
            Some("--") => break,
            Some(option) if option.starts_with('-') => index += 1,
            Some(_) | None => break,
        }
    }
    if args.get(index).is_some_and(|arg| arg == "run") {
        args.split_at(index + 1)
    } else {
        (args, &[])
    }
}

fn utf8_arguments(args: &[OsString]) -> Result<Vec<String>, clap::Error> {
    args.iter()
        .map(|value| {
            value.clone().into_string().map_err(|_| {
                root_command().error(
                    ErrorKind::InvalidUtf8,
                    "this command requires UTF-8 arguments",
                )
            })
        })
        .collect()
}

fn render_clap_error(
    error: &clap::Error,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        write!(stdout, "{error}")?;
        return Ok(ExitCode::SUCCESS);
    }
    write!(stderr, "{error}")?;
    Ok(ExitCode::FAILURE)
}

fn passthrough_values(matches: &ArgMatches) -> Vec<OsString> {
    matches
        .get_many::<OsString>(PASSTHROUGH_ARGS)
        .map_or_else(Vec::new, |values| values.cloned().collect())
}

fn run_sandbox(
    command: wrix_sandbox::command::Command,
    profile_config: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if command == wrix_sandbox::command::Command::Spawn && requests_help(args) {
        wrix_sandbox::command::write_spawn_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    wrix_sandbox::command::run(command, profile_config, args, stdout, stderr)
}

const NOTIFY_HELP: &str = "Run a native command with notification routing.\n\nUsage: wrix notify run -- COMMAND [ARGS ...]\n\nExports WRIX_FOCUS_TARGET to COMMAND and releases registration on exit.\nRegistration failures do not prevent COMMAND from running.\n";
fn run_notify(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || is_help(&args[0]) {
        write!(stdout, "{NOTIFY_HELP}")?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.len() < 3 || args[0] != "run" || args[1] != "--" {
        write!(stderr, "{NOTIFY_HELP}")?;
        return Ok(ExitCode::FAILURE);
    }
    wrix_sandbox::command::run_notification_command(&args[2..]).map_err(io::Error::other)
}

fn run_service(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || is_help(&args[0]) {
        wrix_service::command::write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(command) = wrix_service::command::Top::parse(&args[0]) {
        if requests_help(&args[1..]) {
            wrix_service::command::write_help(stdout)?;
            return Ok(ExitCode::SUCCESS);
        }
        return wrix_service::command::run_top(command, &args[1..], stdout);
    }

    match args[0].as_str() {
        "dolt" => run_dolt(&args[1..], stdout, stderr),
        "cache" => run_cache(&args[1..], stdout, stderr),
        command => {
            writeln!(stderr, "unknown service command: {command}")?;
            wrix_service::command::write_help(stderr)?;
            Ok(ExitCode::FAILURE)
        }
    }
}

fn run_dolt(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || requests_help(args) {
        wrix_service::command::write_dolt_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_service::command::Dolt::parse(&args[0]) {
        if let Err(error) = parse_no_arguments("wrix service dolt", &args[1..]) {
            return render_clap_error(&error, stdout, stderr);
        }
        return wrix_service::command::run_dolt(command, stdout);
    }
    writeln!(stderr, "unknown dolt command: {}", args[0])?;
    wrix_service::command::write_dolt_help(stderr)?;
    Ok(ExitCode::FAILURE)
}

fn run_cache(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || requests_help(args) {
        wrix_cache::command::write_help(stdout).map_err(io::Error::other)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_cache::command::Command::parse(&args[0]) {
        return wrix_cache::command::run(command, &args[1..], stdout).map_err(io::Error::other);
    }
    writeln!(stderr, "unknown cache command: {}", args[0])?;
    wrix_cache::command::write_help(stderr).map_err(io::Error::other)?;
    Ok(ExitCode::FAILURE)
}

fn run_beads(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || requests_help(args) {
        wrix_beads::command::write_help(stdout).map_err(io::Error::other)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_beads::command::Command::parse(&args[0]) {
        if let Err(error) = parse_no_arguments("wrix beads push", &args[1..]) {
            return render_clap_error(&error, stdout, stderr);
        }
        return wrix_beads::command::run(command, stdout, stderr).map_err(io::Error::other);
    }
    writeln!(stderr, "unknown beads command: {}", args[0])?;
    wrix_beads::command::write_help(stderr).map_err(io::Error::other)?;
    Ok(ExitCode::FAILURE)
}

fn parse_no_arguments(name: &'static str, args: &[String]) -> Result<(), clap::Error> {
    ClapCommand::new(name)
        .disable_help_flag(true)
        .try_get_matches_from(std::iter::once(name).chain(args.iter().map(String::as_str)))
        .map(|_matches| ())
}

fn write_delegated_help(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    let Some(topic) = args.first() else {
        write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    };
    match topic.as_str() {
        "notify" => write!(stdout, "{NOTIFY_HELP}")?,
        "run" => wrix_sandbox::command::write_run_help(stdout)?,
        "spawn" => wrix_sandbox::command::write_spawn_help(stdout)?,
        "service" => write_service_topic_help(&args[1..], stdout)?,
        "beads" => wrix_beads::command::write_help(stdout).map_err(io::Error::other)?,
        "init" => crate::init::write_help(stdout)?,
        "workspace" => write!(stdout, "{}", crate::workspace::HELP)?,
        unknown => {
            writeln!(stderr, "unknown help topic: {unknown}")?;
            write_help(stderr)?;
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn write_service_topic_help(args: &[String], stdout: &mut impl Write) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("dolt") => wrix_service::command::write_dolt_help(stdout),
        Some("cache") => wrix_cache::command::write_help(stdout).map_err(io::Error::other),
        Some(_) | None => wrix_service::command::write_help(stdout),
    }
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h" | "help")
}

fn requests_help(args: &[String]) -> bool {
    args.first().is_some_and(|arg| is_help(arg))
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    root_command().write_help(stdout)
}

fn root_command() -> ClapCommand {
    ClapCommand::new("wrix")
        .about("Manage Wrix sandboxes, services, and repository setup.")
        .after_help(wrix_sandbox::command::HOST_SETUP)
        .override_usage("wrix <command>")
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .arg(help_arg())
        .arg(
            Arg::new(PROFILE_CONFIG)
                .long(PROFILE_CONFIG)
                .value_parser(clap::value_parser!(PathBuf))
                .value_name("file")
                .num_args(1)
                .help("Override generated project launch data for run and spawn."),
        )
        .subcommand(
            ClapCommand::new("run")
                .about("Run an interactive sandbox.")
                .override_help(format!("{}\n{}\n", wrix_sandbox::command::RUN_HELP, wrix_sandbox::command::HOST_SETUP))
                .disable_help_flag(true)
                .disable_help_subcommand(true),
        )
        .subcommand(passthrough_command("notify", "Run a native command with notification routing.", NOTIFY_HELP))
        .subcommand(passthrough_command(
            "spawn",
            "Spawn a programmatic sandbox.",
            wrix_sandbox::command::SPAWN_HELP,
        ))
        .subcommand(passthrough_command(
            "service",
            "Manage workspace services.",
            wrix_service::command::HELP,
        ))
        .subcommand(passthrough_command(
            "beads",
            "Manage beads workflows.",
            wrix_beads::command::HELP,
        ))
        .subcommand(passthrough_command(
            "init",
            "Initialize repository Git policy.",
            crate::init::HELP,
        ))
        .subcommand(passthrough_command(
            "workspace",
            "Manage workspace namespace admission.",
            crate::workspace::HELP,
        ))
        .subcommand(passthrough_command(
            "help",
            "Print command help.",
            "Print command help.\n\nUsage: wrix help [command]\n\nOptions:\n  -h, --help  Print help.\n",
        ).arg(help_arg()))
}

fn passthrough_command(name: &'static str, about: &'static str, help: &'static str) -> ClapCommand {
    ClapCommand::new(name)
        .about(about)
        .override_help(help)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(
            Arg::new(PASSTHROUGH_ARGS)
                .value_parser(clap::value_parser!(OsString))
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true),
        )
}

fn help_arg() -> Arg {
    Arg::new(HELP_FLAG)
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help("Print help.")
}

#[cfg(test)]
mod test {
    use std::process::ExitCode;

    use super::{Invocation, parse_root, run};

    #[test]
    fn root_parser_keeps_run_tail_outside_global_parsing() {
        for profile_args in [
            vec!["--profile-config", "run"],
            vec!["--profile-config=run"],
        ] {
            let mut args: Vec<String> = profile_args.iter().map(|arg| (*arg).to_owned()).collect();
            args.push(String::from("run"));
            let tail = [
                "--git-sign",
                "--",
                "--profile-config",
                "agent.json",
                "--help",
                "",
            ];
            args.extend(tail.iter().map(|arg| (*arg).to_owned()));
            let args = args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let parsed = parse_root(&args).unwrap();
            assert_eq!(parsed.profile_config, Some(std::path::PathBuf::from("run")));
            let Some(Invocation::Run(agent_args)) = parsed.invocation else {
                panic!("expected run invocation");
            };
            assert_eq!(agent_args, tail);
        }
    }

    #[test]
    fn root_parser_still_validates_options_before_run() {
        let args = ["--unknown", "run", "/workspace", "--help"].map(String::from);
        let args = args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        assert!(parse_root(&args).is_err());
    }

    #[test]
    fn service_help_lists_public_groups() {
        let args = vec![String::from("service"), String::from("--help")];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(&args, &mut stdout, &mut stderr);
        assert!(matches!(code, Ok(value) if value == ExitCode::SUCCESS));
        assert_eq!(stderr, Vec::<u8>::new());
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("dolt <status|socket|port|host|sandbox-endpoint|attach|gc|wait>"));
        assert!(output.contains("cache <config|status|publish|warm|prune|rotate-key>"));
    }

    #[test]
    fn beads_help_lists_push() {
        let args = vec![String::from("beads"), String::from("--help")];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(&args, &mut stdout, &mut stderr);
        assert!(matches!(code, Ok(value) if value == ExitCode::SUCCESS));
        assert_eq!(stderr, Vec::<u8>::new());
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("push"));
    }
}
