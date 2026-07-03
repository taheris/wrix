mod common;

use std::{fs, path::Path, process::Command};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, mode, public_key, run_command, run_git, set_mode,
    setup_committed_repo, write_ed25519_key, write_fake_gh, write_logging_ssh_keygen,
    write_online_success_git, wrix_command_with_path,
};

#[test]
fn github_deploy_and_signing_keys() -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-deploy-fixtures")
        .tempdir()?;
    let fake_git = write_online_success_git(&fixture.path().join("fake-git"))?;
    let gh_state = fixture.path().join("gh-state");
    let gh_log = fixture.path().join("gh.log");
    let fake_gh = write_fake_gh(&fixture.path().join("fake-gh"), &gh_state, &gh_log)?;
    let ssh_keygen_log = fixture.path().join("ssh-keygen.log");
    let fake_ssh_keygen =
        write_logging_ssh_keygen(&fixture.path().join("fake-ssh-keygen"), &ssh_keygen_log)?;

    let repo = setup_committed_repo("deploy-target", false)?;
    let home = fixture.path().join("home-deploy");
    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("deploy output", &result.stdout, "deploy: true");
    assert_contains("deploy output", &result.stdout, "sign_commits: true");

    let key_dir = home.join(".ssh/deploy_keys");
    let deploy_key = key_dir.join("deploy-key");
    let signing_key = key_dir.join("deploy-key-signing");
    assert!(deploy_key.is_file(), "deploy private key was not generated");
    assert!(
        deploy_key.with_extension("pub").is_file(),
        "deploy public key was not generated",
    );
    assert!(
        signing_key.is_file(),
        "signing private key was not generated",
    );
    assert!(
        signing_key.with_extension("pub").is_file(),
        "signing public key was not generated",
    );
    assert_eq!(mode(&home.join(".ssh"))?, 0o700);
    assert_eq!(mode(&key_dir)?, 0o700);
    assert_eq!(mode(&deploy_key)?, 0o600);
    assert_eq!(mode(&signing_key)?, 0o600);

    let deploy_public = public_key(&deploy_key)?;
    let signing_public = public_key(&signing_key)?;
    assert_eq!(state_value(&gh_state, "deploy_key")?, deploy_public);
    assert_eq!(state_value(&gh_state, "signing_key")?, signing_public);
    let log = fs::read_to_string(&gh_log)?;
    assert_contains(
        "deploy create",
        &log,
        "POST repos/example/deploy-target/keys",
    );
    assert_contains("deploy create", &log, "read_only=false");
    assert_contains("signing create", &log, "POST user/ssh_signing_keys");
    let ssh_keygen_log_content = fs::read_to_string(&ssh_keygen_log)?;
    assert_contains(
        "ssh-keygen deploy invocation",
        &ssh_keygen_log_content,
        "wrix deploy key example/deploy-target",
    );
    assert_contains(
        "ssh-keygen signing invocation",
        &ssh_keygen_log_content,
        "wrix signing key example/deploy-target",
    );

    clear_log(&gh_log)?;
    let before_public = format!("{deploy_public}/{signing_public}");
    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("reuse output", &result.stdout, "deploy: true");
    let after_public = format!("{}/{}", public_key(&deploy_key)?, public_key(&signing_key)?);
    assert_eq!(
        after_public, before_public,
        "deploy run churned key material"
    );
    assert_no_remote_mutation("reuse remote log", &gh_log)?;

    set_mode(&deploy_key, 0o644)?;
    clear_log(&gh_log)?;
    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains("local conflict", &result.stderr, "deploy key");
    assert_contains(
        "local conflict",
        &result.stderr,
        "conflicts with requested deploy provisioning",
    );
    assert_log_empty("local conflict", &gh_log)?;

    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks", "--force"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("force output", &result.stdout, "force: true");
    let deploy_public = public_key(&deploy_key)?;
    assert_eq!(state_value(&gh_state, "deploy_key")?, deploy_public);
    let log = fs::read_to_string(&gh_log)?;
    assert_contains(
        "force deploy delete",
        &log,
        "DELETE repos/example/deploy-target/keys/1",
    );
    assert_contains(
        "force deploy create",
        &log,
        "POST repos/example/deploy-target/keys",
    );

    let conflict_key = fixture.path().join("conflict-key");
    write_ed25519_key(&conflict_key)?;
    let conflict_public = public_key(&conflict_key)?;
    seed_remote_signing(&gh_state, "deploy-key-signing", &conflict_public)?;
    clear_log(&gh_log)?;
    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "remote conflict",
        &result.stderr,
        "remote signing key registration",
    );
    assert_contains(
        "remote conflict",
        &result.stderr,
        "conflicts with requested key",
    );
    assert_no_remote_mutation("remote conflict log", &gh_log)?;

    clear_log(&gh_log)?;
    let result = run_init(
        repo.path(),
        &home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "deploy-key", "--no-hooks", "--force"],
    )?;
    assert_success_with_clean_stderr(&result);
    assert_contains("remote force output", &result.stdout, "force: true");
    let signing_public = public_key(&signing_key)?;
    assert_eq!(state_value(&gh_state, "signing_key")?, signing_public);
    let log = fs::read_to_string(&gh_log)?;
    assert_contains(
        "force signing delete",
        &log,
        "DELETE user/ssh_signing_keys/2",
    );
    assert_contains("force signing create", &log, "POST user/ssh_signing_keys");

    let unsupported_repo = setup_committed_repo("unsupported-remote", false)?;
    run_git(
        unsupported_repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.com:example/unsupported-remote.git",
        ],
    )?;
    clear_log(&gh_log)?;
    let result = run_init(
        unsupported_repo.path(),
        &fixture.path().join("home-unsupported"),
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
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
    assert_log_empty("unsupported remote", &gh_log)?;

    let offline_repo = setup_committed_repo("offline-flag", false)?;
    let offline_home = fixture.path().join("home-offline");
    clear_log(&gh_log)?;
    let result = run_init(
        offline_repo.path(),
        &offline_home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
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
    assert_absent(&offline_home.join(".ssh/deploy_keys/offline-key"));
    assert_log_empty("offline flag", &gh_log)?;

    let config_repo = setup_committed_repo("offline-config", false)?;
    let config_home = fixture.path().join("home-config-offline");
    fs::write(
        config_repo.path().join("wrix.toml"),
        "[wrix.init]\nonline_verify = false\n",
    )?;
    clear_log(&gh_log)?;
    let result = run_init(
        config_repo.path(),
        &config_home,
        &fake_git,
        &fake_gh,
        &fake_ssh_keygen,
        &["--deploy", "--key", "offline-key", "--no-hooks"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "offline config",
        &result.stderr,
        "--deploy requires online verification",
    );
    assert_absent(&config_home.join(".ssh/deploy_keys/offline-key"));
    assert_log_empty("offline config", &gh_log)?;

    Ok(())
}

fn run_init(
    repo: &Path,
    home: &Path,
    fake_git: &Path,
    fake_gh: &Path,
    fake_ssh_keygen: &Path,
    args: &[&str],
) -> TestResult<RunResult> {
    let mut command = init_command(repo, home, fake_git, fake_gh, fake_ssh_keygen)?;
    command.arg("init").args(args);
    run_command(&mut command)
}

fn init_command(
    repo: &Path,
    home: &Path,
    fake_git: &Path,
    fake_gh: &Path,
    fake_ssh_keygen: &Path,
) -> TestResult<Command> {
    let mut command = wrix_command_with_path(repo, &[fake_gh, fake_git, fake_ssh_keygen])?;
    command
        .env("HOME", home)
        .env_remove("WRIX_DEPLOY_KEY")
        .env_remove("WRIX_SIGNING_KEY");
    Ok(command)
}

fn state_value(state_dir: &Path, name: &str) -> TestResult<String> {
    Ok(fs::read_to_string(state_dir.join(name))?.trim().to_owned())
}

fn seed_remote_signing(state_dir: &Path, title: &str, key: &str) -> TestResult {
    fs::write(state_dir.join("signing_id"), "2\n")?;
    fs::write(state_dir.join("signing_title"), format!("{title}\n"))?;
    fs::write(state_dir.join("signing_key"), format!("{key}\n"))?;
    Ok(())
}

fn clear_log(path: &Path) -> TestResult {
    fs::write(path, "")?;
    Ok(())
}

fn assert_no_remote_mutation(label: &str, log_file: &Path) -> TestResult {
    let log = fs::read_to_string(log_file)?;
    assert_not_contains(label, &log, "POST");
    assert_not_contains(label, &log, "DELETE");
    Ok(())
}

fn assert_log_empty(label: &str, log_file: &Path) -> TestResult {
    let log = fs::read_to_string(log_file)?;
    assert!(log.is_empty(), "{label}: unexpected gh API call log: {log}");
    Ok(())
}

fn assert_absent(path: &Path) {
    assert!(!path.exists(), "unexpected file exists: {}", path.display());
}
