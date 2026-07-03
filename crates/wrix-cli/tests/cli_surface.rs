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
    assert_contains("beads help", &beads.stdout, "push");

    let init = run_wrix(&["init", "--help"])?;
    assert_success_with_clean_stderr(&init);
    assert_contains("init help", &init.stdout, "Usage: wrix init");
    assert_contains("init help", &init.stdout, "--deploy");
    assert_contains("init help", &init.stdout, "--offline");

    Ok(())
}

#[test]
fn help_errors_are_non_mutating() -> TestResult {
    let repo = setup_repo("cli-help-errors")?;

    let before = git_config(repo.path())?;
    let init_help = run_wrix_in(repo.path(), &["init", "--help"])?;
    let after = git_config(repo.path())?;
    assert_success_with_clean_stderr(&init_help);
    assert_contains("init help", &init_help.stdout, "Usage: wrix init");
    assert_eq!(before, after, "wrix init --help mutated git config");
    assert!(
        !repo.path().join("wrix.toml").exists(),
        "wrix init --help created wrix.toml"
    );

    let unknown = run_wrix(&["not-a-command"])?;
    assert_failure_with_clean_stdout(&unknown);
    assert_contains("unknown command", &unknown.stderr, "not-a-command");
    assert_contains("unknown command", &unknown.stderr, "Usage: wrix <command>");

    let before = git_config(repo.path())?;
    let deploy_offline = run_wrix_in(repo.path(), &["init", "--deploy", "--offline"])?;
    let after = git_config(repo.path())?;
    assert_failure_with_clean_stdout(&deploy_offline);
    assert_contains(
        "deploy offline",
        &deploy_offline.stderr,
        "--deploy cannot be used with --offline",
    );
    assert_contains("deploy offline", &deploy_offline.stderr, "Usage: wrix init");
    assert_eq!(
        before, after,
        "wrix init --deploy --offline mutated git config"
    );

    let before = git_config(repo.path())?;
    let missing_key = run_wrix_in(repo.path(), &["init", "--key"])?;
    let after = git_config(repo.path())?;
    assert_failure_with_clean_stdout(&missing_key);
    assert_contains("missing key", &missing_key.stderr, "--key requires <name>");
    assert_contains("missing key", &missing_key.stderr, "Usage: wrix init");
    assert_eq!(before, after, "wrix init --key mutated git config");

    let policy = "[wrix.init]\nonline_verify = false\n";
    fs::write(repo.path().join("wrix.toml"), policy)?;
    let before = git_config(repo.path())?;
    let deploy_offline_policy = run_wrix_in(repo.path(), &["init", "--deploy"])?;
    let after = git_config(repo.path())?;
    assert_failure_with_clean_stdout(&deploy_offline_policy);
    assert_contains(
        "deploy offline policy",
        &deploy_offline_policy.stderr,
        "--deploy requires online verification",
    );
    assert_contains(
        "deploy offline policy",
        &deploy_offline_policy.stderr,
        "Usage: wrix init",
    );
    assert_eq!(
        before, after,
        "wrix init --deploy under offline policy mutated git config"
    );
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
