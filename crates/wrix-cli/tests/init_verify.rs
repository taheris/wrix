mod common;

use std::{
    fs,
    path::{Path, PathBuf},
};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, common_git_dir, git_stdout, run_command, run_git, set_mode,
    setup_committed_repo, write_capturing_ssh, write_empty_key, write_tracing_git,
    wrix_command_with_path,
};

#[test]
fn outer_init_verifies_its_own_repository_with_independent_integration_clone() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("online-helper", false)?;
    let integration = clone_integration(repo.path())?;
    run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/online-helper.git",
        ],
    )?;
    let integration_config_path = integration.join(".git/config");
    let integration_config = fs::read(&integration_config_path)?;
    let home = fixture.home("online-helper");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fixture.set_mode("success")?;

    assert_ne!(common_git_dir(repo.path())?, common_git_dir(&integration)?);
    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("online output", &result.stdout, "online_verify: true");
    assert_online_capture(
        &fixture.git_capture_dir,
        &fixture.ssh_capture_dir,
        &repo.path().canonicalize()?,
        &common_git_dir(repo.path())?,
        &deploy_key,
        &home,
        "example/online-helper.git",
    )?;
    assert_eq!(fs::read(integration_config_path)?, integration_config);
    let command = git_stdout(repo.path(), &["config", "--get", "core.sshCommand"])?;
    assert_contains("outer helper config", &command, "wrix/git-ssh");
    Ok(())
}

#[test]
fn init_inside_integration_clone_verifies_that_repository() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("integration-helper", false)?;
    let integration = clone_integration(repo.path())?;
    run_git(
        &integration,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:example/integration-helper.git",
        ],
    )?;
    let outer_config_path = repo.path().join(".git/config");
    let outer_config = fs::read(&outer_config_path)?;
    let home = fixture.home("integration-helper");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(&integration, &home, &deploy_key)?;
    fixture.set_mode("success")?;

    let result = fixture.run_init(
        &integration,
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("integration output", &result.stdout, "online_verify: true");
    assert_online_capture(
        &fixture.git_capture_dir,
        &fixture.ssh_capture_dir,
        &integration,
        &common_git_dir(&integration)?,
        &deploy_key,
        &home,
        "example/integration-helper.git",
    )?;
    assert_eq!(fs::read(outer_config_path)?, outer_config);
    let command = git_stdout(&integration, &["config", "--get", "core.sshCommand"])?;
    assert_contains("integration helper config", &command, "wrix/git-ssh");
    Ok(())
}

#[test]
fn online_verification_rejects_non_github_remote_before_mutation() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("non-github-online", false)?;
    let local_remote = repo.path().join("local-origin.git");
    run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            &local_remote.display().to_string(),
        ],
    )?;
    let config_path = repo.path().join(".git/config");
    let original_config = fs::read(&config_path)?;
    let home = fixture.home("non-github-online");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "non-GitHub online remote",
        &result.stderr,
        "online verification requires a github.com remote",
    );
    assert_eq!(fs::read(config_path)?, original_config);
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn offline_flag_skips_network_verification() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-flag", false)?;
    let home = fixture.home("offline-flag");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("offline output", &result.stdout, "online_verify: false");
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn offline_config_skips_network_verification() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-config", false)?;
    let home = fixture.home("offline-config");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fs::write(
        repo.path().join("wrix.toml"),
        "[init]\nonline_verify = false\n",
    )?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains(
        "config offline output",
        &result.stdout,
        "online_verify: false",
    );
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn offline_verification_rejects_insecure_key_permissions() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-local-check", false)?;
    let home = fixture.home("offline-local-check");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    set_mode(&deploy_key, 0o644)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains("offline local permissions", &result.stderr, "deploy key");
    assert_contains(
        "offline local permissions",
        &result.stderr,
        "no group or other permissions",
    );
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn online_failures_distinguish_host_key_from_authorization() -> TestResult {
    let fixture = VerifyFixture::new()?;

    let host_key_repo = setup_committed_repo("host-key-failure", false)?;
    let host_key_home = fixture.home("host-key-failure");
    let host_key = write_deploy_key(&host_key_home, 0o600)?;
    fixture.prepare(host_key_repo.path(), &host_key_home, &host_key)?;
    fixture.set_mode("host-key")?;
    let result = fixture.run_init(
        host_key_repo.path(),
        &host_key_home,
        &host_key,
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

    let auth_repo = setup_committed_repo("auth-failure", false)?;
    let auth_home = fixture.home("auth-failure");
    let auth_key = write_deploy_key(&auth_home, 0o600)?;
    fixture.prepare(auth_repo.path(), &auth_home, &auth_key)?;
    fixture.set_mode("auth")?;
    let result = fixture.run_init(
        auth_repo.path(),
        &auth_home,
        &auth_key,
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

#[test]
fn worktree_transport_override_fails_verification() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("worktree-transport-override", false)?;
    let home = fixture.home("worktree-transport-override");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    run_git(
        repo.path(),
        &["config", "extensions.worktreeConfig", "true"],
    )?;
    run_git(
        repo.path(),
        &[
            "config",
            "--worktree",
            "core.sshCommand",
            "ssh -o StrictHostKeyChecking=no",
        ],
    )?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "worktree override",
        &result.stderr,
        "core.sshCommand does not match the Wrix common-dir trampoline",
    );
    assert_contains(
        "worktree override",
        &result.stderr,
        "StrictHostKeyChecking=no",
    );
    fixture.assert_no_online_capture();
    Ok(())
}

struct VerifyFixture {
    directory: tempfile::TempDir,
    mode_file: PathBuf,
    git_capture_dir: PathBuf,
    ssh_capture_dir: PathBuf,
    tracing_git: PathBuf,
    fake_ssh: PathBuf,
}

impl VerifyFixture {
    fn new() -> TestResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix("wrix-init-verify-fixtures")
            .tempdir()?;
        let mode_file = directory.path().join("ssh-mode");
        let git_capture_dir = directory.path().join("git-capture");
        let ssh_capture_dir = directory.path().join("ssh-capture");
        let tracing_git =
            write_tracing_git(&directory.path().join("tracing-git"), &git_capture_dir)?;
        let fake_ssh = write_capturing_ssh(
            &directory.path().join("fake-ssh"),
            &mode_file,
            &ssh_capture_dir,
        )?;
        Ok(Self {
            directory,
            mode_file,
            git_capture_dir,
            ssh_capture_dir,
            tracing_git,
            fake_ssh,
        })
    }

    fn home(&self, name: &str) -> PathBuf {
        self.directory.path().join(format!("home-{name}"))
    }

    fn set_mode(&self, mode: &str) -> TestResult {
        fs::write(&self.mode_file, format!("{mode}\n"))?;
        for capture_dir in [&self.git_capture_dir, &self.ssh_capture_dir] {
            if capture_dir.exists() {
                fs::remove_dir_all(capture_dir)?;
            }
            fs::create_dir_all(capture_dir)?;
        }
        Ok(())
    }

    fn assert_no_online_capture(&self) {
        assert_absent(&self.git_capture_dir.join("cwd"));
        assert_absent(&self.ssh_capture_dir.join("cwd"));
    }

    fn prepare(&self, repo: &Path, home: &Path, deploy_key: &Path) -> TestResult {
        let mut command = wrix_command_with_path(repo, &[&self.tracing_git, &self.fake_ssh])?;
        let result = run_command(
            command
                .args([
                    "init",
                    "repo",
                    "--offline",
                    "--no-sign",
                    "--key",
                    "verify-key",
                ])
                .env("HOME", home)
                .env("WRIX_DEPLOY_KEY", deploy_key),
        )?;
        assert_success_with_clean_stderr(&result);
        Ok(())
    }

    fn run_init(
        &self,
        repo: &Path,
        home: &Path,
        deploy_key: &Path,
        args: &[&str],
    ) -> TestResult<RunResult> {
        let mut command = wrix_command_with_path(repo, &[&self.tracing_git, &self.fake_ssh])?;
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
}

fn clone_integration(repo: &Path) -> TestResult<PathBuf> {
    let integration = repo.join(".loom/integration");
    fs::create_dir_all(repo.join(".loom"))?;
    let integration_path = integration.display().to_string();
    run_git(repo, &["clone", "-q", ".", &integration_path])?;
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

fn assert_online_capture(
    git_capture_dir: &Path,
    ssh_capture_dir: &Path,
    expected_cwd: &Path,
    common_dir: &Path,
    deploy_key: &Path,
    home: &Path,
    expected_repo: &str,
) -> TestResult {
    let git_cwd = fs::read_to_string(git_capture_dir.join("cwd"))?;
    assert_eq!(git_cwd.trim(), expected_cwd.display().to_string());
    assert_eq!(
        fs::read_to_string(git_capture_dir.join("args"))?,
        format!("ls-remote\ngit@github.com:{expected_repo}\nHEAD\n"),
    );
    let git_env = fs::read_to_string(git_capture_dir.join("env"))?;
    assert_minimal_online_env("online Git env", &git_env, deploy_key, home);

    let ssh_cwd = fs::read_to_string(ssh_capture_dir.join("cwd"))?;
    assert_eq!(ssh_cwd.trim(), expected_cwd.display().to_string());
    let args = fs::read_to_string(ssh_capture_dir.join("args"))?;
    assert_contains("live ssh args", &args, "BatchMode=yes");
    assert_contains("live ssh args", &args, "IdentitiesOnly=yes");
    assert_contains("live ssh args", &args, "StrictHostKeyChecking=yes");
    assert_contains("live ssh args", &args, "IdentityAgent=none");
    assert_contains("live ssh args", &args, "IdentityFile=none");
    assert_contains(
        "live ssh args",
        &args,
        &format!(
            "UserKnownHostsFile={}",
            common_dir.join("wrix/github_known_hosts").display()
        ),
    );
    assert_contains("live ssh args", &args, &deploy_key.display().to_string());
    assert_contains("live ssh args", &args, "git@github.com");
    assert_contains(
        "live ssh args",
        &args,
        &format!("git-upload-pack '{expected_repo}'"),
    );
    assert_not_contains("live ssh args", &args, "StrictHostKeyChecking=no");

    let ssh_env = fs::read_to_string(ssh_capture_dir.join("env"))?;
    assert_minimal_online_env("online SSH env", &ssh_env, deploy_key, home);
    Ok(())
}

fn assert_minimal_online_env(label: &str, output: &str, deploy_key: &Path, home: &Path) {
    assert_contains(label, output, "GIT_CONFIG_GLOBAL=/dev/null");
    assert_contains(label, output, "GIT_CONFIG_NOSYSTEM=1");
    assert_contains(label, output, "GIT_TERMINAL_PROMPT=0");
    assert_contains(label, output, "GIT_SSH_VARIANT=ssh");
    assert_contains(label, output, &format!("HOME={}", home.display()));
    assert_contains(
        label,
        output,
        &format!("WRIX_DEPLOY_KEY={}", deploy_key.display()),
    );
    assert_not_contains(label, output, "GIT_SSH_COMMAND=");
    assert_not_contains(label, output, "SSH_AUTH_SOCK=");
    assert_not_contains(label, output, "WRIX_SHOULD_NOT_LEAK=");
}

fn assert_absent(path: &Path) {
    assert!(!path.exists(), "unexpected file exists: {}", path.display());
}
