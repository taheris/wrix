mod common;

use std::process::Command;

use common::{
    TestResult, assert_success_with_clean_stderr, git_stdout, run_command, run_git,
    set_isolated_git_env, setup_committed_repo,
};

#[test]
fn git_fixtures_ignore_counted_command_scope_config() -> TestResult {
    assert_unsigned_fixture_commit(&[
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "true"),
        ("GIT_CONFIG_KEY_1", "gpg.program"),
        ("GIT_CONFIG_VALUE_1", "/missing/wrix-fixture-signer"),
    ])
}

#[test]
fn git_fixtures_ignore_command_scope_parameters() -> TestResult {
    assert_unsigned_fixture_commit(&[(
        "GIT_CONFIG_PARAMETERS",
        "'commit.gpgsign=true' 'gpg.program=/missing/wrix-fixture-signer'",
    )])
}

#[test]
fn isolated_git_commands_preserve_repository_signing_config() -> TestResult {
    let repo = setup_committed_repo("wrix-git-local-config", false)?;
    run_git(repo.path(), &["config", "commit.gpgsign", "true"])?;

    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "true"
    );
    Ok(())
}

fn assert_unsigned_fixture_commit(environment: &[(&str, &str)]) -> TestResult {
    let repo = setup_committed_repo("wrix-git-command-config", false)?;
    let mut command = Command::new("git");
    command
        .current_dir(repo.path())
        .args(["commit", "--allow-empty", "-qm", "isolated fixture commit"])
        .envs(environment.iter().copied());
    set_isolated_git_env(&mut command);

    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert!(
        !git_stdout(repo.path(), &["cat-file", "-p", "HEAD"])?.contains("\ngpgsig "),
        "fixture commit must not inherit signing requirements"
    );
    Ok(())
}
