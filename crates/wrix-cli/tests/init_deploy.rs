mod common;

use std::{fs, path::Path, path::PathBuf, process::Command};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, mode, public_key, run_command, run_git, set_mode,
    setup_committed_repo, write_ed25519_key, write_fake_gh, write_logging_ssh_keygen,
    write_online_success_git, wrix_command_with_path,
};

#[test]
fn github_deploy_and_signing_keys() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-target", false)?;
    let home = fixture.home("initial");

    let result = fixture.run_init(
        repo.path(),
        &home,
        &["--deploy", "--key", "deploy-key", "--no-hooks"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("deploy output", &result.stdout, "deploy: true");
    assert_contains("deploy output", &result.stdout, "sign_commits: true");
    let deploy_key = home.join(".ssh/deploy_keys/deploy-key");
    let signing_key = home.join(".ssh/deploy_keys/deploy-key-signing");
    for path in [
        deploy_key.as_path(),
        deploy_key.with_extension("pub").as_path(),
        signing_key.as_path(),
        signing_key.with_extension("pub").as_path(),
    ] {
        assert!(
            path.is_file(),
            "generated key is missing: {}",
            path.display()
        );
    }
    assert_eq!(mode(&home.join(".ssh"))?, 0o700);
    assert_eq!(mode(&home.join(".ssh/deploy_keys"))?, 0o700);
    assert_eq!(mode(&deploy_key)?, 0o600);
    assert_eq!(mode(&signing_key)?, 0o600);

    assert_eq!(fixture.state_value("deploy_key")?, public_key(&deploy_key)?);
    assert_eq!(
        fixture.state_value("signing_key")?,
        public_key(&signing_key)?,
    );
    let log = fs::read_to_string(&fixture.gh_log)?;
    assert_contains(
        "deploy create",
        &log,
        "POST repos/example/deploy-target/keys",
    );
    assert_contains("deploy create", &log, "read_only=false");
    assert_contains("signing create", &log, "POST user/ssh_signing_keys");
    let keygen_log = fs::read_to_string(&fixture.ssh_keygen_log)?;
    assert_contains(
        "ssh-keygen deploy invocation",
        &keygen_log,
        "wrix deploy key example/deploy-target",
    );
    assert_contains(
        "ssh-keygen signing invocation",
        &keygen_log,
        "wrix signing key example/deploy-target",
    );
    Ok(())
}

#[test]
fn matching_deploy_keys_are_reused() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-reuse", false)?;
    let home = fixture.home("reuse");
    let args = ["--deploy", "--key", "deploy-key", "--no-hooks"];
    assert_success_with_clean_stderr(&fixture.run_init(repo.path(), &home, &args)?);
    let deploy_key = home.join(".ssh/deploy_keys/deploy-key");
    let signing_key = home.join(".ssh/deploy_keys/deploy-key-signing");
    let before = format!("{}/{}", public_key(&deploy_key)?, public_key(&signing_key)?);
    fixture.clear_gh_log()?;

    let result = fixture.run_init(repo.path(), &home, &args)?;

    assert_success_with_clean_stderr(&result);
    assert_eq!(
        before,
        format!("{}/{}", public_key(&deploy_key)?, public_key(&signing_key)?),
        "repeated deploy provisioning churned key material",
    );
    fixture.assert_no_remote_mutation("reuse remote log")?;
    Ok(())
}

#[test]
fn legacy_setup_signing_registration_is_reused_without_rotation() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("legacy-deploy-setup", false)?;
    let home = fixture.home("legacy-setup");
    let args = ["--deploy", "--key", "deploy-key", "--no-hooks"];
    assert_success_with_clean_stderr(&fixture.run_init(repo.path(), &home, &args)?);
    let deploy_key = home.join(".ssh/deploy_keys/deploy-key");
    let signing_key = home.join(".ssh/deploy_keys/deploy-key-signing");
    let original = (fs::read(&deploy_key)?, fs::read(&signing_key)?);
    fixture.seed_remote_signing("signing-deploy-key", &public_key(&signing_key)?)?;
    fixture.clear_gh_log()?;

    assert_success_with_clean_stderr(&fixture.run_init(repo.path(), &home, &args)?);

    assert_eq!((fs::read(&deploy_key)?, fs::read(&signing_key)?), original);
    assert_eq!(fixture.state_value("signing_title")?, "signing-deploy-key");
    fixture.assert_no_remote_mutation("legacy signing registration")?;
    Ok(())
}

#[test]
fn local_key_conflict_requires_force() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-local-conflict", false)?;
    let home = fixture.home("local-conflict");
    let args = ["--deploy", "--key", "deploy-key", "--no-hooks"];
    assert_success_with_clean_stderr(&fixture.run_init(repo.path(), &home, &args)?);
    let deploy_key = home.join(".ssh/deploy_keys/deploy-key");
    set_mode(&deploy_key, 0o644)?;
    fixture.clear_gh_log()?;

    let rejected = fixture.run_init(repo.path(), &home, &args)?;

    assert_failure_with_clean_stdout(&rejected);
    assert_contains("local conflict", &rejected.stderr, "deploy key");
    assert_contains(
        "local conflict",
        &rejected.stderr,
        "conflicts with requested deploy provisioning",
    );
    fixture.assert_gh_log_empty("local conflict")?;

    let replaced = fixture.run_init(
        repo.path(),
        &home,
        &["--deploy", "--key", "deploy-key", "--no-hooks", "--force"],
    )?;
    assert_success_with_clean_stderr(&replaced);
    assert_contains("force output", &replaced.stdout, "force: true");
    assert_eq!(fixture.state_value("deploy_key")?, public_key(&deploy_key)?);
    let log = fs::read_to_string(&fixture.gh_log)?;
    assert_contains(
        "force deploy delete",
        &log,
        "DELETE repos/example/deploy-local-conflict/keys/1",
    );
    assert_contains(
        "force deploy create",
        &log,
        "POST repos/example/deploy-local-conflict/keys",
    );
    Ok(())
}

#[test]
fn remote_key_conflict_requires_force() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-remote-conflict", false)?;
    let home = fixture.home("remote-conflict");
    let args = ["--deploy", "--key", "deploy-key", "--no-hooks"];
    assert_success_with_clean_stderr(&fixture.run_init(repo.path(), &home, &args)?);
    let conflict_key = fixture.directory.path().join("conflict-key");
    write_ed25519_key(&conflict_key)?;
    fixture.seed_remote_signing("deploy-key-signing", &public_key(&conflict_key)?)?;
    fixture.clear_gh_log()?;

    let rejected = fixture.run_init(repo.path(), &home, &args)?;

    assert_failure_with_clean_stdout(&rejected);
    assert_contains(
        "remote conflict",
        &rejected.stderr,
        "remote signing key registration",
    );
    assert_contains(
        "remote conflict",
        &rejected.stderr,
        "conflicts with requested key",
    );
    fixture.assert_no_remote_mutation("remote conflict log")?;

    fixture.clear_gh_log()?;
    let replaced = fixture.run_init(
        repo.path(),
        &home,
        &["--deploy", "--key", "deploy-key", "--no-hooks", "--force"],
    )?;
    assert_success_with_clean_stderr(&replaced);
    assert_contains("remote force output", &replaced.stdout, "force: true");
    assert_eq!(
        fixture.state_value("signing_key")?,
        public_key(&home.join(".ssh/deploy_keys/deploy-key-signing"))?,
    );
    let log = fs::read_to_string(&fixture.gh_log)?;
    assert_contains(
        "force signing delete",
        &log,
        "DELETE user/ssh_signing_keys/2",
    );
    assert_contains("force signing create", &log, "POST user/ssh_signing_keys");
    Ok(())
}

#[test]
fn unsupported_deploy_remote_fails_before_api_mutation() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("unsupported-remote", false)?;
    run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.com:example/unsupported-remote.git",
        ],
    )?;
    let home = fixture.home("unsupported");
    fixture.clear_gh_log()?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &[
            "--deploy",
            "--key",
            "unsupported-key",
            "--no-sign",
            "--no-hooks",
        ],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "unsupported remote",
        &result.stderr,
        "supports only github.com remotes",
    );
    fixture.assert_gh_log_empty("unsupported remote")?;
    Ok(())
}

#[test]
fn deploy_offline_flag_fails_before_key_or_api_mutation() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-offline-flag", false)?;
    let home = fixture.home("offline-flag");
    fixture.clear_gh_log()?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &[
            "--deploy",
            "--offline",
            "--key",
            "offline-key",
            "--no-hooks",
        ],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "offline flag",
        &result.stderr,
        "--deploy cannot be used with --offline",
    );
    assert_absent(&home.join(".ssh/deploy_keys/offline-key"));
    fixture.assert_gh_log_empty("offline flag")?;
    Ok(())
}

#[test]
fn deploy_offline_config_fails_before_key_or_api_mutation() -> TestResult {
    let fixture = DeployFixture::new()?;
    let repo = setup_committed_repo("deploy-offline-config", false)?;
    let home = fixture.home("offline-config");
    fs::write(
        repo.path().join("wrix.toml"),
        "[wrix.init]\nonline_verify = false\n",
    )?;
    fixture.clear_gh_log()?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &["--deploy", "--key", "offline-key", "--no-hooks"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "offline config",
        &result.stderr,
        "--deploy requires online verification",
    );
    assert_absent(&home.join(".ssh/deploy_keys/offline-key"));
    fixture.assert_gh_log_empty("offline config")?;
    Ok(())
}

struct DeployFixture {
    directory: tempfile::TempDir,
    fake_git: PathBuf,
    fake_gh: PathBuf,
    fake_ssh_keygen: PathBuf,
    gh_state: PathBuf,
    gh_log: PathBuf,
    ssh_keygen_log: PathBuf,
}

impl DeployFixture {
    fn new() -> TestResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix("wrix-init-deploy-fixtures")
            .tempdir()?;
        let fake_git = write_online_success_git(&directory.path().join("fake-git"))?;
        let gh_state = directory.path().join("gh-state");
        let gh_log = directory.path().join("gh.log");
        let fake_gh = write_fake_gh(&directory.path().join("fake-gh"), &gh_state, &gh_log)?;
        let ssh_keygen_log = directory.path().join("ssh-keygen.log");
        let fake_ssh_keygen =
            write_logging_ssh_keygen(&directory.path().join("fake-ssh-keygen"), &ssh_keygen_log)?;
        Ok(Self {
            directory,
            fake_git,
            fake_gh,
            fake_ssh_keygen,
            gh_state,
            gh_log,
            ssh_keygen_log,
        })
    }

    fn home(&self, name: &str) -> PathBuf {
        self.directory.path().join(format!("home-{name}"))
    }

    fn run_init(&self, repo: &Path, home: &Path, args: &[&str]) -> TestResult<RunResult> {
        let mut command = self.init_command(repo, home)?;
        command.arg("init").args(args);
        run_command(&mut command)
    }

    fn init_command(&self, repo: &Path, home: &Path) -> TestResult<Command> {
        let mut command = wrix_command_with_path(
            repo,
            &[&self.fake_gh, &self.fake_git, &self.fake_ssh_keygen],
        )?;
        command
            .env("HOME", home)
            .env_remove("WRIX_DEPLOY_KEY")
            .env_remove("WRIX_SIGNING_KEY");
        Ok(command)
    }

    fn state_value(&self, name: &str) -> TestResult<String> {
        Ok(fs::read_to_string(self.gh_state.join(name))?
            .trim()
            .to_owned())
    }

    fn seed_remote_signing(&self, title: &str, key: &str) -> TestResult {
        fs::write(self.gh_state.join("signing_id"), "2\n")?;
        fs::write(self.gh_state.join("signing_title"), format!("{title}\n"))?;
        fs::write(self.gh_state.join("signing_key"), format!("{key}\n"))?;
        Ok(())
    }

    fn clear_gh_log(&self) -> TestResult {
        fs::write(&self.gh_log, "")?;
        Ok(())
    }

    fn assert_no_remote_mutation(&self, label: &str) -> TestResult {
        let log = fs::read_to_string(&self.gh_log)?;
        assert_not_contains(label, &log, "POST");
        assert_not_contains(label, &log, "DELETE");
        Ok(())
    }

    fn assert_gh_log_empty(&self, label: &str) -> TestResult {
        let log = fs::read_to_string(&self.gh_log)?;
        assert!(log.is_empty(), "{label}: unexpected gh API call log: {log}");
        Ok(())
    }
}

fn assert_absent(path: &Path) {
    assert!(!path.exists(), "unexpected file exists: {}", path.display());
}
