mod common;

use std::{fs, path::Path};

use common::{
    TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, common_git_dir, git_command, git_command_with_path,
    git_stdout, mode, public_key, run_command, setup_committed_repo, write_ed25519_key,
    wrix_command,
};

#[test]
fn signing_is_opt_in() -> TestResult {
    let repo = setup_committed_repo("signing-opt-in", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-opt-in")
        .tempdir()?;
    let home = fixture.path().join("home");
    let deploy_key = home.join(".ssh/deploy_keys/signing-key");
    let signing_key = home.join(".ssh/deploy_keys/signing-key-signing");
    write_ed25519_key(&deploy_key)?;
    let common_dir = common_git_dir(repo.path())?;

    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "signing-key"])
        .env("HOME", &home)
        .env(
            "WRIX_SIGNING_KEY",
            fixture.path().join("absent-signing-key"),
        );
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains("default signing output", &result.stdout, "sign: false");
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "false"
    );
    assert!(!signing_key.exists());
    assert!(!common_dir.join("wrix/allowed_signers").exists());
    assert!(!repo.path().join("wrix.toml").exists());
    assert!(
        !run_command(&mut git_command(
            repo.path(),
            &["config", "--get", "gpg.ssh.program"]
        ))?
        .status
        .success()
    );
    run_git_with_signing_env(
        repo.path(),
        &["commit", "--allow-empty", "-qm", "unsigned commit"],
        &home,
    )?;
    assert_eq!(
        git_stdout(repo.path(), &["log", "-1", "--format=%G?"])?,
        "N"
    );

    write_ed25519_key(&signing_key)?;
    for use_policy in [false, true] {
        if use_policy {
            fs::write(repo.path().join("wrix.toml"), "[wrix.git]\nsign = true\n")?;
        }
        let mut command = wrix_command(repo.path())?;
        command
            .arg("init")
            .args(["--offline", "--key", "signing-key"])
            .env("HOME", &home);
        if !use_policy {
            command.arg("--sign");
        }
        let result = run_command(&mut command)?;
        assert_success_with_clean_stderr(&result);
        assert_contains("opt-in signing output", &result.stdout, "sign: true");
        assert_signing_config(repo.path(), &home, &signing_key)?;
        run_git_with_signing_env(
            repo.path(),
            &["commit", "--allow-empty", "-qm", "signed commit"],
            &home,
        )?;
        run_git_with_signing_env(repo.path(), &["verify-commit", "HEAD"], &home)?;
    }
    Ok(())
}

#[test]
fn signing_key_env_must_point_to_file() -> TestResult {
    let repo = setup_committed_repo("signing-missing-env", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-missing-env")
        .tempdir()?;
    let home = fixture.path().join("home");
    fs::create_dir_all(&home)?;
    let before = fs::read(repo.path().join(".git/config"))?;
    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--sign", "--key", "missing-key"])
        .env("HOME", &home)
        .env("WRIX_SIGNING_KEY", fixture.path().join("absent-key"));

    let result = run_command(&mut command)?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "missing WRIX_SIGNING_KEY",
        &result.stderr,
        "WRIX_SIGNING_KEY does not point at a file",
    );
    assert_eq!(before, fs::read(repo.path().join(".git/config"))?);
    assert!(!repo.path().join(".git/wrix").exists());
    Ok(())
}

#[test]
fn fallback_signing_key_is_required() -> TestResult {
    let repo = setup_committed_repo("signing-missing-home", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-missing-home")
        .tempdir()?;
    let home = fixture.path().join("home");
    fs::create_dir_all(&home)?;
    let before = fs::read(repo.path().join(".git/config"))?;
    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--sign", "--key", "missing-key"])
        .env("HOME", &home);

    let result = run_command(&mut command)?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "missing home signing key",
        &result.stderr,
        "fallback signing key does not exist",
    );
    assert_eq!(before, fs::read(repo.path().join(".git/config"))?);
    assert!(!repo.path().join(".git/wrix").exists());
    Ok(())
}

#[test]
fn no_sign_flag_disables_signing() -> TestResult {
    let repo = setup_committed_repo("signing-disabled-flag", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-no-sign")
        .tempdir()?;
    let home = fixture.path().join("home");
    write_ed25519_key(&home.join(".ssh/deploy_keys/no-sign-key"))?;
    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "no-sign-key", "--no-sign"])
        .env("HOME", &home);

    let result = run_command(&mut command)?;

    assert_success_with_clean_stderr(&result);
    assert_contains("--no-sign output", &result.stdout, "sign: false");
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "false"
    );
    Ok(())
}

#[test]
fn signing_config_opt_out_disables_signing() -> TestResult {
    let repo = setup_committed_repo("signing-disabled-config", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-config-opt-out")
        .tempdir()?;
    let home = fixture.path().join("home");
    write_ed25519_key(&home.join(".ssh/deploy_keys/config-key"))?;
    fs::write(repo.path().join("wrix.toml"), "[wrix.git]\nsign = false\n")?;
    let mut command = wrix_command(repo.path())?;
    command
        .arg("init")
        .args(["--offline", "--key", "config-key"])
        .env("HOME", &home);

    let result = run_command(&mut command)?;

    assert_success_with_clean_stderr(&result);
    assert_contains("config disabled output", &result.stdout, "sign: false");
    assert_eq!(
        git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
        "false"
    );
    Ok(())
}

#[test]
fn sign_flags_override_repository_policy() -> TestResult {
    let repo = setup_committed_repo("signing-flag-precedence", false)?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-signing-flag-precedence")
        .tempdir()?;
    let home = fixture.path().join("home");
    let signing_key = home.join(".ssh/deploy_keys/signing-key-signing");
    write_ed25519_key(&home.join(".ssh/deploy_keys/signing-key"))?;
    write_ed25519_key(&signing_key)?;

    for (policy_sign, flag, expected) in [(false, "--sign", "true"), (true, "--no-sign", "false")] {
        let policy = format!("[wrix.git]\ndeploy = false\nsign = {policy_sign}\n");
        fs::write(repo.path().join("wrix.toml"), &policy)?;
        let mut command = wrix_command(repo.path())?;
        command
            .arg("init")
            .args(["--offline", "--key", "signing-key", flag])
            .env("HOME", &home);
        if policy_sign {
            command.env(
                "WRIX_SIGNING_KEY",
                fixture.path().join("absent-signing-key"),
            );
        }
        let result = run_command(&mut command)?;
        assert_success_with_clean_stderr(&result);
        assert_contains(
            "flag signing output",
            &result.stdout,
            &format!("sign: {expected}"),
        );
        assert_eq!(
            git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
            expected
        );
        assert_eq!(fs::read_to_string(repo.path().join("wrix.toml"))?, policy);
        run_git_with_signing_env(
            repo.path(),
            &["commit", "--allow-empty", "-qm", "override commit"],
            &home,
        )?;
        if policy_sign {
            assert_eq!(
                git_stdout(repo.path(), &["log", "-1", "--format=%G?"])?,
                "N"
            );
        } else {
            run_git_with_signing_env(repo.path(), &["verify-commit", "HEAD"], &home)?;
        }
    }
    Ok(())
}

fn assert_signing_config(repo: &Path, home: &Path, signing_key: &Path) -> TestResult {
    for (key, expected) in [
        ("gpg.format", "ssh"),
        ("commit.gpgsign", "true"),
        ("gpg.ssh.program", "wrix-git-sign"),
        ("gpg.ssh.allowedSignersFile", "wrix/allowed_signers"),
        ("user.signingkey", "wrix/signing-key/signing-key-signing"),
    ] {
        let value = git_stdout(repo, &["config", "--get", key])?;
        assert_eq!(value, expected);
        assert_stable_config_value(key, &value, repo, home);
    }
    let allowed_signers = common_git_dir(repo)?.join("wrix/allowed_signers");
    assert_eq!(mode(&allowed_signers)?, 0o600);
    assert_contains(
        "allowed signers",
        &fs::read_to_string(&allowed_signers)?,
        &format!("wrix-test@example.invalid {}", public_key(signing_key)?),
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
        result.stderr
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
