mod common;

use std::{
    fs,
    path::{Path, PathBuf},
};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, git_stdout, run_command, run_git, set_mode,
    setup_committed_repo, write_capturing_git, write_empty_key, wrix_command_with_path,
};

#[test]
fn online_and_offline_verification() -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-verify-fixtures")
        .tempdir()?;
    let mode_file = fixture.path().join("git-mode");
    let capture_dir = fixture.path().join("online-capture");
    let fake_git = write_capturing_git(&fixture.path().join("fake-git"), &mode_file, &capture_dir)?;

    let repo = setup_committed_repo("online-success", false)?;
    let integration = add_integration_worktree(repo.path())?;
    let home = fixture.path().join("home-online");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fs::write(&mode_file, "success\n")?;
    reset_capture(&capture_dir)?;
    let result = run_init(
        repo.path(),
        &home,
        &deploy_key,
        &fake_git,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("online output", &result.stdout, "online_verify: true");
    assert_online_capture(&capture_dir, &integration, &deploy_key, &home)?;
    let command = git_stdout(&integration, &["config", "--get", "core.sshCommand"])?;
    assert_contains("linked helper config", &command, "wrix/git-ssh");

    let repo = setup_committed_repo("offline-flag", false)?;
    let home = fixture.path().join("home-offline");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fs::write(&mode_file, "fail-if-online\n")?;
    reset_capture(&capture_dir)?;
    let result = run_init(
        repo.path(),
        &home,
        &deploy_key,
        &fake_git,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("offline output", &result.stdout, "online_verify: false");
    assert_absent(&capture_dir.join("cwd"));

    let config_repo = setup_committed_repo("offline-config", false)?;
    fs::write(
        config_repo.path().join("wrix.toml"),
        "[wrix.init]\nonline_verify = false\n",
    )?;
    reset_capture(&capture_dir)?;
    let result = run_init(
        config_repo.path(),
        &home,
        &deploy_key,
        &fake_git,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "config offline output",
        &result.stdout,
        "online_verify: false",
    );
    assert_absent(&capture_dir.join("cwd"));

    let bad_repo = setup_committed_repo("offline-local-check", false)?;
    let bad_home = fixture.path().join("home-bad-perms");
    let bad_key = write_deploy_key(&bad_home, 0o644)?;
    reset_capture(&capture_dir)?;
    let result = run_init(
        bad_repo.path(),
        &bad_home,
        &bad_key,
        &fake_git,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains("offline local permissions", &result.stderr, "deploy key");
    assert_contains(
        "offline local permissions",
        &result.stderr,
        "no group or other permissions",
    );
    assert_absent(&capture_dir.join("cwd"));

    let repo = setup_committed_repo("host-key-failure", false)?;
    let home = fixture.path().join("home-host-key");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fs::write(&mode_file, "host-key\n")?;
    let result = run_init(
        repo.path(),
        &home,
        &deploy_key,
        &fake_git,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "host-key failure",
        &result.stderr,
        "online verification failed host-key verification",
    );
    assert_not_contains(
        "host-key failure",
        &result.stderr,
        "authentication or repository authorization failed",
    );

    let repo = setup_committed_repo("auth-failure", false)?;
    let home = fixture.path().join("home-auth");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fs::write(&mode_file, "auth\n")?;
    let result = run_init(
        repo.path(),
        &home,
        &deploy_key,
        &fake_git,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "auth failure",
        &result.stderr,
        "authentication or repository authorization failed",
    );
    assert_not_contains(
        "auth failure",
        &result.stderr,
        "failed host-key verification",
    );

    Ok(())
}

fn add_integration_worktree(repo: &Path) -> TestResult<PathBuf> {
    let integration = repo.join(".loom/integration");
    fs::create_dir_all(repo.join(".loom"))?;
    let integration_path = integration.display().to_string();
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            &integration_path,
            "-b",
            "loom-integration",
        ],
    )?;
    Ok(integration.canonicalize()?)
}

fn write_deploy_key(home: &Path, mode: u32) -> TestResult<PathBuf> {
    let key = home.join(".ssh/deploy_keys/verify-key");
    write_empty_key(&key)?;
    set_mode(&home.join(".ssh"), 0o700)?;
    set_mode(&home.join(".ssh/deploy_keys"), 0o700)?;
    set_mode(&key, mode)?;
    Ok(key)
}

fn reset_capture(capture_dir: &Path) -> TestResult {
    if capture_dir.exists() {
        fs::remove_dir_all(capture_dir)?;
    }
    fs::create_dir_all(capture_dir)?;
    Ok(())
}

fn run_init(
    repo: &Path,
    home: &Path,
    deploy_key: &Path,
    fake_git: &Path,
    args: &[&str],
) -> TestResult<RunResult> {
    let mut command = wrix_command_with_path(repo, &[fake_git])?;
    command
        .arg("init")
        .args(args)
        .env("GIT_SSH_COMMAND", "ambient ssh")
        .env("SSH_AUTH_SOCK", home.join("agent.sock"))
        .env("WRIX_SHOULD_NOT_LEAK", "1")
        .env("HOME", home)
        .env("WRIX_DEPLOY_KEY", deploy_key);
    run_command(&mut command)
}

fn assert_online_capture(
    capture_dir: &Path,
    expected_cwd: &Path,
    deploy_key: &Path,
    home: &Path,
) -> TestResult {
    let cwd = fs::read_to_string(capture_dir.join("cwd"))?;
    assert_eq!(cwd.trim(), expected_cwd.display().to_string());
    let args = fs::read_to_string(capture_dir.join("args"))?;
    assert_contains("ls-remote args", &args, "ls-remote");
    assert_contains("ls-remote args", &args, "origin");
    assert_contains("ls-remote args", &args, "HEAD");
    let env_output = fs::read_to_string(capture_dir.join("env"))?;
    assert_contains("online env", &env_output, "GIT_CONFIG_GLOBAL=/dev/null");
    assert_contains("online env", &env_output, "GIT_CONFIG_NOSYSTEM=1");
    assert_contains("online env", &env_output, "GIT_TERMINAL_PROMPT=0");
    assert_contains("online env", &env_output, "GIT_SSH_VARIANT=ssh");
    assert_contains(
        "online env",
        &env_output,
        &format!("HOME={}", home.display()),
    );
    assert_contains(
        "online env",
        &env_output,
        &format!("WRIX_DEPLOY_KEY={}", deploy_key.display()),
    );
    assert_not_contains("online env", &env_output, "GIT_SSH_COMMAND=");
    assert_not_contains("online env", &env_output, "SSH_AUTH_SOCK=");
    assert_not_contains("online env", &env_output, "WRIX_SHOULD_NOT_LEAK=");
    Ok(())
}

fn assert_absent(path: &Path) {
    assert!(!path.exists(), "unexpected file exists: {}", path.display());
}
