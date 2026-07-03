mod common;

use std::{fs, path::Path};

use common::{
    TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, common_git_dir, git_command_with_path, git_stdout, mode,
    public_key, run_command, setup_committed_repo, write_ed25519_key, wrix_command,
};

#[test]
fn signing_required_by_default() -> TestResult {
    let repo = setup_committed_repo("signing-default", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-fixtures")
        .tempdir()?;
    let home = fixture.path().join("home-signing");
    let deploy_key = home.join(".ssh/deploy_keys/signing-key");
    let signing_key = home.join(".ssh/deploy_keys/signing-key-signing");
    write_ed25519_key(&deploy_key)?;
    write_ed25519_key(&signing_key)?;

    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "signing-key"])
        .env("HOME", &home);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "default signing output",
        &result.stdout,
        "sign_commits: true",
    );

    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "gpg.format"])?,
        "ssh"
    );
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "true",
    );
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "gpg.ssh.program"])?,
        "wrix-git-sign",
    );
    assert_eq!(
        git_stdout(
            repo.path(),
            &["config", "--get", "gpg.ssh.allowedSignersFile"],
        )?,
        "wrix/allowed_signers",
    );
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "user.signingkey"])?,
        "wrix/signing-key/signing-key-signing",
    );
    assert_stable_config_value(
        "gpg.ssh.program",
        &git_stdout(repo.path(), &["config", "--get", "gpg.ssh.program"])?,
        repo.path(),
        &home,
    );
    assert_stable_config_value(
        "gpg.ssh.allowedSignersFile",
        &git_stdout(
            repo.path(),
            &["config", "--get", "gpg.ssh.allowedSignersFile"],
        )?,
        repo.path(),
        &home,
    );
    assert_stable_config_value(
        "user.signingkey",
        &git_stdout(repo.path(), &["config", "--get", "user.signingkey"])?,
        repo.path(),
        &home,
    );

    let common_dir = common_git_dir(repo.path())?;
    let allowed_signers = common_dir.join("wrix/allowed_signers");
    assert!(
        allowed_signers.is_file(),
        "allowed signers file was not generated at {}",
        allowed_signers.display(),
    );
    assert_eq!(mode(&allowed_signers)?, 0o600);
    let public_key = public_key(&signing_key)?;
    assert_contains(
        "allowed signers",
        &fs::read_to_string(&allowed_signers)?,
        &format!("wrix-test@example.invalid {public_key}"),
    );

    fs::write(repo.path().join("signed.txt"), "signed\n")?;
    run_git_with_signing_env(repo.path(), &["add", "signed.txt"], &home)?;
    run_git_with_signing_env(repo.path(), &["commit", "-qm", "signed commit"], &home)?;
    run_git_with_signing_env(repo.path(), &["verify-commit", "HEAD"], &home)?;

    let integration = repo.path().join(".loom/integration");
    fs::create_dir_all(repo.path().join(".loom"))?;
    run_git_with_signing_env(
        repo.path(),
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "worktree",
            "add",
            "-q",
            integration.to_str().expect("integration path is UTF-8"),
            "-b",
            "loom-integration",
        ],
        &home,
    )?;
    run_git_with_signing_env(&integration, &["verify-commit", "HEAD"], &home)?;

    let missing_repo = setup_committed_repo("signing-missing-env", false)?;
    let missing_home = fixture.path().join("home-missing-env");
    fs::create_dir_all(&missing_home)?;
    let mut command = wrix_command(missing_repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "missing-key"])
        .env("HOME", &missing_home)
        .env(
            "WRIX_SIGNING_KEY",
            fixture.path().join("absent-signing-key"),
        );
    let result = run_command(&mut command)?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "missing WRIX_SIGNING_KEY",
        &result.stderr,
        "WRIX_SIGNING_KEY does not point at a file",
    );

    let missing_repo = setup_committed_repo("signing-missing-home", false)?;
    let missing_home = fixture.path().join("home-missing-home");
    fs::create_dir_all(&missing_home)?;
    let mut command = wrix_command(missing_repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "missing-key"])
        .env("HOME", &missing_home);
    let result = run_command(&mut command)?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "missing home signing key",
        &result.stderr,
        "fallback signing key does not exist",
    );

    let no_sign_repo = setup_committed_repo("signing-disabled-flag", false)?;
    let no_sign_home = fixture.path().join("home-no-sign");
    write_ed25519_key(&no_sign_home.join(".ssh/deploy_keys/no-sign-key"))?;
    let mut command = wrix_command(no_sign_repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "no-sign-key", "--no-sign"])
        .env("HOME", &no_sign_home);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains("--no-sign output", &result.stdout, "sign_commits: false");
    assert_eq!(
        git_stdout(no_sign_repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "false",
    );

    let config_repo = setup_committed_repo("signing-disabled-config", false)?;
    let config_home = fixture.path().join("home-config-no-sign");
    write_ed25519_key(&config_home.join(".ssh/deploy_keys/config-key"))?;
    fs::write(
        config_repo.path().join("wrix.toml"),
        "[wrix.git]\nsign_commits = false\n",
    )?;
    let mut command = wrix_command(config_repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "config-key"])
        .env("HOME", &config_home);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "config disabled output",
        &result.stdout,
        "sign_commits: false",
    );
    assert_eq!(
        git_stdout(config_repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "false",
    );

    Ok(())
}

fn run_git_with_signing_env(repo: &Path, args: &[&str], home: &Path) -> TestResult {
    let mut command = git_command_with_path(repo, args, &[])?;
    command
        .env("HOME", home)
        .env_remove("WRIX_SIGNING_KEY")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_EMAIL");
    let result = run_command(&mut command)?;
    assert!(
        result.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        result.stdout,
        result.stderr,
    );
    Ok(())
}

fn assert_stable_config_value(label: &str, value: &str, repo: &Path, home: &Path) {
    assert_not_contains(label, value, &repo.display().to_string());
    assert_not_contains(label, value, &home.display().to_string());
    assert_not_contains(label, value, "/nix/store");
    assert_not_contains(label, value, "/etc/wrix/keys");
    assert_not_contains(label, value, "/workspace");
    assert_not_contains(label, value, ".ssh/deploy_keys");
}
