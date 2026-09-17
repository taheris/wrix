use std::{
    fs,
    path::Path,
    process::{Command, ExitStatus},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn root_and_subcommand_help() -> TestResult {
    let root = run_wrix(&["--help"])?;
    assert_success_with_clean_stderr(&root);
    for expected in [
        "run",
        "spawn",
        "service",
        "beads",
        "init",
        "Run an interactive sandbox.",
        "Spawn a programmatic sandbox.",
        "Manage workspace services.",
        "Manage beads workflows.",
        "Initialize repository Git policy.",
    ] {
        assert_contains("root help", &root.stdout, expected);
    }

    let help_command = run_wrix(&["help"])?;
    assert_success_with_clean_stderr(&help_command);
    assert_contains(
        "help command",
        &help_command.stdout,
        "Usage: wrix <command>",
    );

    let run = run_wrix(&["run", "--help"])?;
    assert_success_with_clean_stderr(&run);
    assert_contains(
        "run help",
        &run.stdout,
        "Usage: wrix [--profile-config <file>] run",
    );

    let spawn = run_wrix(&["spawn", "--help"])?;
    assert_success_with_clean_stderr(&spawn);
    assert_contains(
        "spawn help",
        &spawn.stdout,
        "Usage: wrix [--profile-config <file>] spawn",
    );

    let service = run_wrix(&["help", "service"])?;
    assert_success_with_clean_stderr(&service);
    assert_contains(
        "service help",
        &service.stdout,
        "Usage: wrix service <command>",
    );
    assert_contains(
        "service help",
        &service.stdout,
        "dolt <status|socket|port|host|attach|gc|wait>",
    );

    let dolt = run_wrix(&["service", "dolt", "--help"])?;
    assert_success_with_clean_stderr(&dolt);
    assert_contains(
        "dolt help",
        &dolt.stdout,
        "Usage: wrix service dolt <command>",
    );

    let beads = run_wrix(&["beads", "--help"])?;
    assert_success_with_clean_stderr(&beads);
    assert_contains("beads help", &beads.stdout, "Usage: wrix beads <command>");
    assert_contains(
        "beads help",
        &beads.stdout,
        "push  Synchronize session-close beads state.",
    );

    let init = run_wrix(&["init", "--help"])?;
    assert_success_with_clean_stderr(&init);
    assert_contains("init help", &init.stdout, "Usage: wrix init");
    assert_contains("init help", &init.stdout, "--deploy");
    assert_contains("init help", &init.stdout, "--offline");

    Ok(())
}

#[test]
fn public_flags_have_descriptions() -> TestResult {
    let cases = [
        (
            "root",
            vec!["--help"],
            vec!["-h, --help", "--profile-config <file>"],
        ),
        (
            "run",
            vec!["run", "--help"],
            vec!["--profile-config <file>", "-h, --help"],
        ),
        (
            "spawn",
            vec!["spawn", "--stdio", "--help"],
            vec![
                "--profile-config <file>",
                "--spawn-config <file>",
                "--stdio",
                "-h, --help",
            ],
        ),
        (
            "service",
            vec!["service", "start", "--no-cache", "--help"],
            vec!["--no-cache", "-h, --help"],
        ),
        (
            "dolt",
            vec!["service", "dolt", "status", "--help"],
            vec!["-h, --help"],
        ),
        (
            "cache",
            vec!["service", "cache", "warm", "--help"],
            vec!["--checks", "-h, --help"],
        ),
        ("beads", vec!["beads", "push", "--help"], vec!["-h, --help"]),
        (
            "init",
            vec!["init", "--offline", "--help"],
            vec![
                "--deploy",
                "--key <name>",
                "--remote <name>",
                "--offline",
                "--no-sign",
                "--no-hooks",
                "--force",
                "-h, --help",
            ],
        ),
        ("help", vec!["help", "--help"], vec!["-h, --help"]),
    ];

    for (label, args, expected) in cases {
        let result = run_wrix(&args)?;
        assert_success_with_clean_stderr(&result);
        assert_described_options(label, &result.stdout, &expected);
    }
    Ok(())
}

#[test]
fn init_help_is_non_mutating() -> TestResult {
    let repo = setup_repo("cli-init-help")?;
    let before = git_config(repo.path())?;

    let result = run_wrix_in(repo.path(), &["init", "--help"])?;

    assert_success_with_clean_stderr(&result);
    assert_contains("init help", &result.stdout, "Usage: wrix init");
    assert_eq!(before, git_config(repo.path())?);
    assert!(
        !repo.path().join("wrix.toml").exists(),
        "wrix init --help created wrix.toml"
    );
    Ok(())
}

#[test]
fn unknown_root_command_reports_usage() -> TestResult {
    let result = run_wrix(&["not-a-command"])?;

    assert_failure_with_clean_stdout(&result);
    assert_contains("unknown command", &result.stderr, "not-a-command");
    assert_contains("unknown command", &result.stderr, "Usage: wrix <command>");
    Ok(())
}

#[test]
fn missing_init_flag_value_is_non_mutating() -> TestResult {
    let repo = setup_repo("cli-missing-init-value")?;
    let before = git_config(repo.path())?;

    let result = run_wrix_in(repo.path(), &["init", "--key"])?;

    assert_failure_with_clean_stdout(&result);
    assert_contains("missing key", &result.stderr, "--key requires <name>");
    assert_contains("missing key", &result.stderr, "Usage: wrix init");
    assert_eq!(before, git_config(repo.path())?);
    Ok(())
}

#[test]
fn deploy_offline_flags_are_non_mutating() -> TestResult {
    let repo = setup_repo("cli-deploy-offline-flags")?;
    let before = git_config(repo.path())?;

    let result = run_wrix_in(repo.path(), &["init", "--deploy", "--offline"])?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "deploy offline",
        &result.stderr,
        "--deploy cannot be used with --offline",
    );
    assert_contains("deploy offline", &result.stderr, "Usage: wrix init");
    assert_eq!(before, git_config(repo.path())?);
    Ok(())
}

#[test]
fn deploy_under_offline_policy_is_non_mutating() -> TestResult {
    let repo = setup_repo("cli-deploy-offline-policy")?;
    let policy = "[wrix.init]\nonline_verify = false\n";
    fs::write(repo.path().join("wrix.toml"), policy)?;
    let before = git_config(repo.path())?;

    let result = run_wrix_in(repo.path(), &["init", "--deploy"])?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "deploy offline policy",
        &result.stderr,
        "--deploy requires online verification",
    );
    assert_contains("deploy offline policy", &result.stderr, "Usage: wrix init");
    assert_eq!(before, git_config(repo.path())?);
    assert_eq!(fs::read_to_string(repo.path().join("wrix.toml"))?, policy);
    Ok(())
}

struct RunResult {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_wrix(args: &[&str]) -> TestResult<RunResult> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
    command.args(args);
    run_wrix_command(&mut command)
}

fn run_wrix_in(cwd: &Path, args: &[&str]) -> TestResult<RunResult> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
    command.current_dir(cwd).args(args);
    run_wrix_command(&mut command)
}

fn run_wrix_command(command: &mut Command) -> TestResult<RunResult> {
    let output = command.output()?;
    Ok(RunResult {
        status: output.status,
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

fn setup_repo(name: &str) -> TestResult<tempfile::TempDir> {
    let repo = tempfile::Builder::new().prefix(name).tempdir()?;
    run_git(repo.path(), &["init", "-q"])?;
    run_git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example/cli-help-errors.git",
        ],
    )?;
    Ok(repo)
}

fn run_git(cwd: &Path, args: &[&str]) -> TestResult {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

fn git_config(repo: &Path) -> TestResult<String> {
    Ok(fs::read_to_string(repo.join(".git").join("config"))?)
}

fn assert_success_with_clean_stderr(result: &RunResult) {
    assert!(result.status.success(), "stderr: {}", result.stderr);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {}",
        result.stderr
    );
}

fn assert_failure_with_clean_stdout(result: &RunResult) {
    assert!(!result.status.success(), "command unexpectedly succeeded");
    assert!(
        result.stdout.is_empty(),
        "unexpected stdout: {}",
        result.stdout
    );
}

fn assert_contains(label: &str, haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "{label}: missing {needle:?} in {haystack:?}",
    );
}

fn assert_described_options(label: &str, help: &str, expected: &[&str]) {
    const OPTIONS_HEADING: &str = "Options:\n";
    let options_start = help.find(OPTIONS_HEADING);
    assert!(
        options_start.is_some(),
        "{label}: help has no Options section: {help:?}",
    );
    let options_start = options_start.map_or(help.len(), |index| index + OPTIONS_HEADING.len());
    let options = &help[options_start..];
    let mut actual = Vec::new();
    for line in options.lines().filter(|line| !line.trim().is_empty()) {
        let option = line.trim();
        let description_start = option.find("  ");
        assert!(
            description_start.is_some(),
            "{label}: option has no description: {line:?}",
        );
        let description_start = description_start.unwrap_or(option.len());
        let (syntax, description) = option.split_at(description_start);
        assert!(
            !description.trim().is_empty(),
            "{label}: option {syntax:?} has a blank description",
        );
        actual.push(syntax);
    }
    assert_eq!(actual, expected, "{label}: unexpected public option set");
}
