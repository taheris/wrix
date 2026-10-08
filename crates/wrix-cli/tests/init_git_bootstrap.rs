mod common;

use std::{fs, path::Path};

use common::{
    TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, common_git_dir, git_stdout, mode, run_command, run_git,
    setup_committed_repo, write_ed25519_key, write_empty_key, write_fake_ssh, wrix_command,
};

#[test]
fn outer_init_leaves_independent_loom_integration_clone_unchanged() -> TestResult {
    let repo = setup_committed_repo("common-config", false)?;
    let integration = repo.path().join(".loom/integration");
    fs::create_dir_all(repo.path().join(".loom"))?;
    run_git(
        repo.path(),
        &[
            "clone",
            "-q",
            ".",
            integration.to_str().expect("integration path is UTF-8"),
        ],
    )?;
    let integration_config_path = integration.join(".git/config");
    let integration_config = fs::read(&integration_config_path)?;
    assert_ne!(common_git_dir(repo.path())?, common_git_dir(&integration)?);

    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-common-fixtures")
        .tempdir()?;
    let home = fixture.path().join("home");
    let deploy_key = home.join(".ssh/deploy_keys/common-key");
    let signing_key = home.join(".ssh/deploy_keys/common-key-signing");
    write_empty_key(&deploy_key)?;
    write_ed25519_key(&signing_key)?;

    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--sign", "--key", "common-key"])
        .env("HOME", &home);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "init output",
        &result.stdout,
        "wrix init: repository policy resolved",
    );
    assert_eq!(fs::read(integration_config_path)?, integration_config);

    let common_dir = common_git_dir(repo.path())?;
    for (key, expected) in [
        ("core.sshCommand", None),
        ("gpg.format", Some("ssh")),
        ("gpg.ssh.program", Some("wrix-git-sign")),
        ("gpg.ssh.allowedSignersFile", Some("wrix/allowed_signers")),
        (
            "user.signingkey",
            Some("wrix/signing-key/common-key-signing"),
        ),
        ("commit.gpgsign", Some("true")),
    ] {
        let value = git_stdout(repo.path(), &["config", "--get", key])?;
        if let Some(expected) = expected {
            assert_eq!(value, expected, "unexpected {key}");
        }
        assert_stable_config_value(key, &value, repo.path(), &home);
        let origin = git_stdout(repo.path(), &["config", "--show-origin", "--get", key])?;
        let common_config = format!("file:{}", common_dir.join("config").display());
        assert!(
            origin.contains(&common_config) || origin.starts_with("file:.git/config"),
            "outer {key} origin was not the repository common config: {origin}",
        );
    }
    let command = git_stdout(repo.path(), &["config", "--get", "core.sshCommand"])?;
    assert_contains("ssh command", &command, "git rev-parse --git-common-dir");

    let state_dir = common_dir.join("wrix");
    assert!(
        state_dir.join("git-ssh").is_file(),
        "missing transport helper at {}",
        state_dir.join("git-ssh").display(),
    );
    assert_eq!(mode(&state_dir.join("git-ssh"))?, 0o700);
    assert!(
        state_dir.join("github_known_hosts").is_file(),
        "missing pinned GitHub known-hosts at {}",
        state_dir.join("github_known_hosts").display(),
    );
    assert_eq!(mode(&state_dir.join("github_known_hosts"))?, 0o600);
    assert!(
        state_dir.join("allowed_signers").is_file(),
        "missing allowed signers at {}",
        state_dir.join("allowed_signers").display(),
    );
    assert_eq!(mode(&state_dir.join("allowed_signers"))?, 0o600);

    Ok(())
}

#[test]
fn strict_context_aware_ssh_helper() -> TestResult {
    let repo = setup_committed_repo("strict-helper", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-strict-fixtures")
        .tempdir()?;
    let home = fixture.path().join("home");
    let env_key = fixture.path().join("env-deploy-key");
    let home_key = home.join(".ssh/deploy_keys/strict-key");
    write_empty_key(&env_key)?;
    write_empty_key(&home_key)?;
    fs::create_dir_all(home.join(".ssh"))?;
    fs::write(
        home.join(".ssh/config"),
        format!(
            "Host github.com\n  IdentityFile {}\n",
            fixture.path().join("ambient-key").display(),
        ),
    )?;
    common::set_mode(&home.join(".ssh/config"), 0o600)?;

    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--no-sign", "--key", "strict-key"])
        .env("HOME", &home)
        .env("WRIX_DEPLOY_KEY", &env_key);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "init output",
        &result.stdout,
        "wrix init: repository policy resolved",
    );

    let common_dir = common_git_dir(repo.path())?;
    let state_dir = common_dir.join("wrix");
    let helper = state_dir.join("git-ssh");
    let known_hosts = state_dir.join("github_known_hosts");
    assert_eq!(mode(&state_dir)?, 0o700);
    assert_eq!(mode(&helper)?, 0o700);
    assert_eq!(mode(&known_hosts)?, 0o600);
    assert_contains(
        "known hosts",
        &fs::read_to_string(&known_hosts)?,
        "github.com ssh-ed25519",
    );

    let fake_bin = write_fake_ssh(&fixture.path().join("fake-bin"))?;
    let env_key = env_key.canonicalize()?;
    let home_key = home_key.canonicalize()?;

    let capture = fixture.path().join("ssh-env.args");
    let mut command = helper_command(&helper, &fake_bin, &home, &capture)?;
    command.env("WRIX_DEPLOY_KEY", &env_key);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    let args = fs::read_to_string(&capture)?;
    assert_contains("env deploy key", &args, &env_key.display().to_string());
    assert_not_contains("env deploy key", &args, &home_key.display().to_string());
    assert_strict_helper_args("env deploy key", &args, &env_key, &known_hosts);

    let capture = fixture.path().join("ssh-home.args");
    let mut command = helper_command(&helper, &fake_bin, &home, &capture)?;
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    let args = fs::read_to_string(&capture)?;
    assert_contains("home deploy key", &args, &home_key.display().to_string());
    assert_not_contains("home deploy key", &args, &env_key.display().to_string());
    assert_strict_helper_args("home deploy key", &args, &home_key, &known_hosts);

    let missing_home = fixture.path().join("missing-home");
    fs::create_dir_all(&missing_home)?;
    let capture = fixture.path().join("ssh-missing.args");
    let mut command = helper_command(&helper, &fake_bin, &missing_home, &capture)?;
    let result = run_command(&mut command)?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "missing deploy key",
        &result.stderr,
        "no deploy key resolved",
    );
    assert!(
        !capture.exists(),
        "helper invoked ssh after failing deploy-key resolution",
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

fn helper_command(
    helper: &Path,
    fake_bin: &Path,
    home: &Path,
    capture: &Path,
) -> TestResult<std::process::Command> {
    let mut command = std::process::Command::new(helper);
    command
        .arg("git@github.com")
        .arg("git-upload-pack 'example/strict-helper.git'")
        .env("HOME", home)
        .env("PATH", path_with_fake_bin(fake_bin)?)
        .env("WRIX_TEST_CAPTURE", capture)
        .env_remove("WRIX_DEPLOY_KEY");
    Ok(command)
}

fn path_with_fake_bin(fake_bin: &Path) -> TestResult<std::ffi::OsString> {
    let mut paths = vec![fake_bin.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    Ok(std::env::join_paths(paths)?)
}

fn assert_strict_helper_args(label: &str, args: &str, key_path: &Path, known_hosts: &Path) {
    assert_contains(label, args, "-F\n/dev/null");
    assert_contains(label, args, "BatchMode=yes");
    assert_contains(label, args, "IdentitiesOnly=yes");
    assert_contains(label, args, "StrictHostKeyChecking=yes");
    assert_contains(
        label,
        args,
        &format!("UserKnownHostsFile={}", known_hosts.display()),
    );
    assert_contains(label, args, "GlobalKnownHostsFile=/dev/null");
    assert_contains(label, args, "IdentityAgent=none");
    assert_contains(label, args, "IdentityFile=none");
    assert_contains(label, args, &format!("-i\n{}", key_path.display()));
    assert_not_contains(label, args, "StrictHostKeyChecking=no");
}
