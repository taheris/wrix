use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match run(&args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "wrix: {error}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String], stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<ExitCode> {
    if args.is_empty() || is_help(&args[0]) {
        write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }

    match args[0].as_str() {
        "run" => run_sandbox(wrix_sandbox::command::Command::Run, &args[1..], stdout),
        "spawn" => run_sandbox(wrix_sandbox::command::Command::Spawn, &args[1..], stdout),
        "service" => run_service(&args[1..], stdout, stderr),
        "beads" => run_beads(&args[1..], stdout, stderr),
        command => unknown_root(command, stdout, stderr),
    }
}

fn run_sandbox(
    command: wrix_sandbox::command::Command,
    args: &[String],
    stdout: &mut impl Write,
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
    wrix_sandbox::command::run(command, stdout)
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

fn unknown_root(
    command: &str,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    writeln!(stderr, "unknown command: {command}")?;
    write_help(stdout)?;
    Ok(ExitCode::FAILURE)
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h" | "help")
}

fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Manage wrix sandboxes and workspace services.\n\nUsage: wrix <command>\n\nCommands:\n  run\n  spawn\n  service\n  beads\n",
    )
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
        assert!(output.contains("dolt <status|socket|port|host|attach|gc>"));
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
