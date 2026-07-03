mod common;

use std::process::Command;

use common::{
    TestResult, assert_contains, assert_success_with_clean_stderr, run_command, write_fake_ssh,
    write_online_success_git,
};

#[test]
fn command_fixtures_have_expected_observable_contract() -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-command-fixtures")
        .tempdir()?;
    let fake_git = write_online_success_git(&fixture.path().join("fake-git"))?;
    let fake_ssh = write_fake_ssh(&fixture.path().join("fake-ssh"))?;

    let real_git = run_command(Command::new("git").arg("--version"))?;
    let fake_git_version = run_command(Command::new(fake_git.join("git")).arg("--version"))?;
    assert_success_with_clean_stderr(&real_git);
    assert_success_with_clean_stderr(&fake_git_version);
    assert_eq!(fake_git_version.stdout, real_git.stdout);

    let fake_ls_remote = run_command(
        Command::new(fake_git.join("git"))
            .arg("ls-remote")
            .arg("origin")
            .arg("HEAD"),
    )?;
    assert_success_with_clean_stderr(&fake_ls_remote);
    assert_contains(
        "fake ls-remote",
        &fake_ls_remote.stdout,
        "0123456789012345678901234567890123456789\tHEAD",
    );

    let capture = fixture.path().join("ssh.args");
    let fake_ssh_output = run_command(
        Command::new(fake_ssh.join("ssh"))
            .arg("-G")
            .arg("github.com")
            .env("WRIX_TEST_CAPTURE", &capture),
    )?;
    assert_success_with_clean_stderr(&fake_ssh_output);
    assert_eq!(std::fs::read_to_string(capture)?, "-G\ngithub.com\n");

    Ok(())
}
