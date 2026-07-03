use std::{
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
        Some(Invocation::Service(args)) => run_service(&args, stdout, stderr),
        Some(Invocation::Beads(args)) => run_beads(&args, stdout, stderr),
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
    Service(Vec<String>),
    Beads(Vec<String>),
    Init(Vec<String>),
    Help(Vec<String>),
}

fn parse_root(args: &[String]) -> Result<RootInvocation, clap::Error> {
    let matches = root_command()
        .try_get_matches_from(std::iter::once("wrix").chain(args.iter().map(String::as_str)))?;
    let profile_config = matches.get_one::<String>(PROFILE_CONFIG).map(PathBuf::from);
    let invocation = match matches.subcommand() {
        Some(("run", matches)) => Some(Invocation::Run(passthrough_values(matches))),
        Some(("spawn", matches)) => Some(Invocation::Spawn(passthrough_values(matches))),
        Some(("service", matches)) => Some(Invocation::Service(passthrough_values(matches))),
        Some(("beads", matches)) => Some(Invocation::Beads(passthrough_values(matches))),
        Some(("init", matches)) => Some(Invocation::Init(passthrough_values(matches))),
        Some(("help", matches)) => Some(Invocation::Help(passthrough_values(matches))),
        Some(_) | None => None,
    };
    Ok(RootInvocation {
        profile_config,
        invocation,
    })
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

fn passthrough_values(matches: &ArgMatches) -> Vec<String> {
    matches
        .get_many::<String>(PASSTHROUGH_ARGS)
        .map_or_else(Vec::new, |values| values.cloned().collect())
}

fn run_sandbox(
    command: wrix_sandbox::command::Command,
    profile_config: Option<PathBuf>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.first().is_some_and(|arg| is_help(arg)) {
        match command {
            wrix_sandbox::command::Command::Run => wrix_sandbox::command::write_run_help(stdout)?,
            wrix_sandbox::command::Command::Spawn => {
                wrix_sandbox::command::write_spawn_help(stdout)?;
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    wrix_sandbox::command::run(command, profile_config, args, stdout, stderr)
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
        if args.get(1).is_some_and(|arg| is_help(arg)) {
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
    if args.is_empty() || is_help(&args[0]) {
        wrix_service::command::write_dolt_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_service::command::Dolt::parse(&args[0]) {
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
    if args.is_empty() || is_help(&args[0]) {
        wrix_cache::command::write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_cache::command::Command::parse(&args[0]) {
        return wrix_cache::command::run(command, &args[1..], stdout);
    }
    writeln!(stderr, "unknown cache command: {}", args[0])?;
    wrix_cache::command::write_help(stderr)?;
    Ok(ExitCode::FAILURE)
}

fn run_beads(
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty() || is_help(&args[0]) {
        wrix_beads::command::write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(command) = wrix_beads::command::Command::parse(&args[0]) {
        return wrix_beads::command::run(command, stdout, stderr);
    }
    writeln!(stderr, "unknown beads command: {}", args[0])?;
    wrix_beads::command::write_help(stderr)?;
    Ok(ExitCode::FAILURE)
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
        "run" => wrix_sandbox::command::write_run_help(stdout)?,
        "spawn" => wrix_sandbox::command::write_spawn_help(stdout)?,
        "service" => write_service_topic_help(&args[1..], stdout)?,
        "beads" => wrix_beads::command::write_help(stdout)?,
        "init" => crate::init::write_help(stdout)?,
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
        Some("cache") => wrix_cache::command::write_help(stdout),
        Some(_) | None => wrix_service::command::write_help(stdout),
    }
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h" | "help")
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    root_command().write_help(stdout)
}

fn root_command() -> ClapCommand {
    ClapCommand::new("wrix")
        .about("Manage Wrix sandboxes, services, and repository setup.")
        .override_usage("wrix <command>")
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .arg(help_arg())
        .arg(
            Arg::new(PROFILE_CONFIG)
                .long(PROFILE_CONFIG)
                .value_name("file")
                .num_args(1)
                .help("Read launcher defaults from a profile config."),
        )
        .subcommand(passthrough_command(
            "run",
            "Run an interactive sandbox.",
            wrix_sandbox::command::RUN_HELP,
        ))
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
            "help",
            "Print command help.",
            "Print command help.\n\nUsage: wrix help [command]\n",
        ))
}

fn passthrough_command(name: &'static str, about: &'static str, help: &'static str) -> ClapCommand {
    ClapCommand::new(name)
        .about(about)
        .override_help(help)
        .disable_help_flag(true)
        .arg(help_arg())
        .arg(
            Arg::new(PASSTHROUGH_ARGS)
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

    use super::run;

    #[test]
    fn service_help_lists_public_groups() {
        let args = vec![String::from("service"), String::from("--help")];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(&args, &mut stdout, &mut stderr);
        assert!(matches!(code, Ok(value) if value == ExitCode::SUCCESS));
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("dolt <status|socket|port|host|attach|gc|wait>"));
        assert!(output.contains("cache <status|publish|warm|prune|rotate-key>"));
    }

    #[test]
    fn beads_help_lists_push() {
        let args = vec![String::from("beads"), String::from("--help")];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(&args, &mut stdout, &mut stderr);
        assert!(matches!(code, Ok(value) if value == ExitCode::SUCCESS));
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("push"));
    }
}
