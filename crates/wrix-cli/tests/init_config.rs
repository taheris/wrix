mod common;

use std::{fs, path::Path, process::Command};

use common::{
    TestResult, assert_contains, assert_failure_with_clean_stdout,
    assert_success_with_clean_stderr, run_command, run_git, setup_repo, write_ed25519_key,
    write_empty_key, write_fake_gh, write_logging_ssh_keygen, write_online_success_git,
    write_prek_hooks, wrix_command_with_path,
};

#[test]
fn defaults_and_overrides() -> TestResult {
    let repo = setup_repo("config-defaults")?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-config-fixtures")
        .tempdir()?;
    let home = fixture.path().join("home");
    let deploy_key = fixture.path().join("deploy-key");
    let signing_key = fixture.path().join("signing-key");
    let hooks = write_prek_hooks(&fixture.path().join("hooks"))?;
    let fake_git = write_online_success_git(&fixture.path().join("fake-git"))?;
    let profile_config = fixture.path().join("profile-config.json");

    fs::write(repo.path().join(".pre-commit-config.yaml"), "repos:\n")?;
    fs::write(
        &profile_config,
        r#"{"security":{"deploy_key":"profile-key"}}"#,
    )?;
    write_empty_key(&deploy_key)?;
    write_ed25519_key(&signing_key)?;

    let derived_key = format!(
        "{}-devhost",
        repo.path()
            .file_name()
            .and_then(|value| value.to_str())
            .expect("temp repo basename is UTF-8"),
    );
    let mut command = init_command(
        repo.path(),
        &home,
        &deploy_key,
        &signing_key,
        &hooks,
        &[fake_git.as_path()],
    )?;
    command.arg("init");
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_policy_line(
        "derived defaults",
        &result.stdout,
        "deploy_key",
        &derived_key,
    );
    assert_policy_line("derived defaults", &result.stdout, "sign", "false");
    assert_policy_line("derived defaults", &result.stdout, "remote", "origin");
    assert_policy_line("derived defaults", &result.stdout, "prek_hooks", "true");
    assert_policy_line("derived defaults", &result.stdout, "online_verify", "true");
    assert!(
        !repo.path().join("wrix.toml").exists(),
        "wrix init created wrix.toml for default behavior",
    );

    let mut command = init_command(
        repo.path(),
        &home,
        &deploy_key,
        &signing_key,
        &hooks,
        &[fake_git.as_path()],
    )?;
    command
        .arg("--profile-config")
        .arg(&profile_config)
        .arg("init");
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_policy_line(
        "profile config",
        &result.stdout,
        "deploy_key",
        "profile-key",
    );
    assert_policy_line("profile config", &result.stdout, "remote", "origin");

    run_git(
        repo.path(),
        &[
            "remote",
            "add",
            "upstream",
            "git@github.com:example/upstream.git",
        ],
    )?;
    fs::write(
        repo.path().join("wrix.toml"),
        r"wrix.git = { deploy_key = 'toml-key', sign = false, remote = 'upstream' }
wrix.init = { prek_hooks = false, online_verify = false }
",
    )?;
    let mut command = init_command(repo.path(), &home, &deploy_key, &signing_key, &hooks, &[])?;
    command
        .arg("--profile-config")
        .arg(&profile_config)
        .arg("init");
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_policy_line("wrix.toml", &result.stdout, "deploy_key", "toml-key");
    assert_policy_line("wrix.toml", &result.stdout, "sign", "false");
    assert_policy_line("wrix.toml", &result.stdout, "remote", "upstream");
    assert_policy_line("wrix.toml", &result.stdout, "prek_hooks", "false");
    assert_policy_line("wrix.toml", &result.stdout, "online_verify", "false");

    fs::write(
        repo.path().join("wrix.toml"),
        r#"[wrix.git]
deploy_key = "toml-key"
sign = true
remote = "upstream"

[wrix.init]
prek_hooks = true
online_verify = true
"#,
    )?;
    let mut command = init_command(repo.path(), &home, &deploy_key, &signing_key, &hooks, &[])?;
    command
        .arg("--profile-config")
        .arg(&profile_config)
        .arg("init")
        .args([
            "--key",
            "flag-key",
            "--remote",
            "origin",
            "--offline",
            "--no-sign",
            "--no-hooks",
            "--force",
        ]);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_policy_line("flag overrides", &result.stdout, "deploy_key", "flag-key");
    assert_policy_line("flag overrides", &result.stdout, "sign", "false");
    assert_policy_line("flag overrides", &result.stdout, "remote", "origin");
    assert_policy_line("flag overrides", &result.stdout, "prek_hooks", "false");
    assert_policy_line("flag overrides", &result.stdout, "online_verify", "false");
    assert_policy_line("flag overrides", &result.stdout, "force", "true");

    Ok(())
}

#[test]
fn git_grants_are_independent_and_default_false() -> TestResult {
    let repo = setup_repo("config-independent-grants")?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-independent-grants")
        .tempdir()?;
    let home = fixture.path().join("home");
    let deploy_key = fixture.path().join("deploy-key");
    let signing_key = fixture.path().join("signing-key");
    write_empty_key(&deploy_key)?;
    write_ed25519_key(&signing_key)?;
    let profile = fixture.path().join("profile.json");
    fs::write(&profile, r#"{"security":{"deploy_key":"profile-key"}}"#)?;

    for (policy, deploy, sign) in [
        (None, false, false),
        (
            Some("[wrix.git]\ndeploy_key = 'policy-key'\n"),
            false,
            false,
        ),
        (Some("[wrix.git]\ndeploy = true\n"), true, false),
        (Some("[wrix.git]\nsign = true\n"), false, true),
        (
            Some("[wrix.git]\ndeploy = false\nsign = false\n"),
            false,
            false,
        ),
        (
            Some("[wrix.git]\ndeploy = false\nsign = true\n"),
            false,
            true,
        ),
        (
            Some("[wrix.git]\ndeploy = true\nsign = false\n"),
            true,
            false,
        ),
        (Some("[wrix.git]\ndeploy = true\nsign = true\n"), true, true),
    ] {
        if let Some(policy) = policy {
            fs::write(repo.path().join("wrix.toml"), policy)?;
        }
        let grants = wrix_core::repository_policy::read(repo.path())?.git;
        assert_eq!(
            (grants.deploy_enabled(), grants.sign_enabled()),
            (deploy, sign)
        );
        let mut command = wrix_command_with_path(repo.path(), &[])?;
        command
            .args([
                "--profile-config",
                profile.to_str().expect("fixture path is UTF-8"),
                "init",
                "--offline",
                "--no-hooks",
            ])
            .env("HOME", &home)
            .env("WRIX_DEPLOY_KEY", &deploy_key)
            .env("WRIX_SIGNING_KEY", &signing_key);
        let result = run_command(&mut command)?;
        assert_success_with_clean_stderr(&result);
        assert_policy_line(
            "grant policy",
            &result.stdout,
            "sign",
            if sign { "true" } else { "false" },
        );
        assert_policy_line("grant policy", &result.stdout, "deploy", "false");
        assert_eq!(
            common::git_stdout(repo.path(), &["config", "--get", "commit.gpgsign"])?,
            sign.to_string()
        );
        assert!(!home.join(".ssh/deploy_keys").exists());
        if let Some(policy) = policy {
            assert_eq!(fs::read_to_string(repo.path().join("wrix.toml"))?, policy);
        } else {
            assert!(!repo.path().join("wrix.toml").exists());
        }
    }
    Ok(())
}

#[test]
fn invalid_and_retired_git_grants_are_rejected() -> TestResult {
    let repo = setup_repo("config-invalid-grants")?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-invalid-grants")
        .tempdir()?;
    let home = fixture.path().join("home");
    let gh_log = fixture.path().join("gh.log");
    let keygen_log = fixture.path().join("keygen.log");
    let fake_gh = write_fake_gh(
        &fixture.path().join("fake-gh"),
        &fixture.path().join("gh-state"),
        &gh_log,
    )?;
    let fake_keygen = write_logging_ssh_keygen(&fixture.path().join("fake-keygen"), &keygen_log)?;
    let before = fs::read(repo.path().join(".git/config"))?;

    for policy in [
        "[wrix.git]\ndeploy = 'true'\n",
        "[wrix.git]\nsign = 'false'\n",
        "[wrix.git]\ndeploy = 1\n",
        "[wrix.git]\nsign = 0\n",
        "[wrix.git]\ndeploy = []\n",
        "[wrix.git]\nsign = {}\n",
        "wrix.git = false\n",
        "[wrix.git]\nsign_commits = true\n",
        "[wrix.git]\nsign_commits = false\nsign = false\n",
        "[wrix.git]\nsign = true\nsign = false\n",
        "[wrix.git]\ndeploy_key = 'first'\ndeploy_key = 'second'\n",
        "[wrix.git\n",
    ] {
        fs::write(repo.path().join("wrix.toml"), policy)?;
        let mut command = wrix_command_with_path(repo.path(), &[&fake_gh, &fake_keygen])?;
        command
            .args([
                "init",
                "--deploy",
                "--sign",
                "--key",
                "policy-key",
                "--no-hooks",
            ])
            .env("HOME", &home);
        let result = run_command(&mut command)?;
        assert_failure_with_clean_stdout(&result);
        assert_contains("invalid grant", &result.stderr, "invalid repository policy");
        if policy.contains("sign_commits") {
            assert_contains(
                "retired grant",
                &result.stderr,
                "unknown field `sign_commits`",
            );
        }
        assert_eq!(before, fs::read(repo.path().join(".git/config"))?);
        assert_eq!(policy, fs::read_to_string(repo.path().join("wrix.toml"))?);
        assert!(!repo.path().join(".git/wrix").exists());
        assert!(!home.exists());
        assert_eq!(fs::read_to_string(&gh_log)?, "");
        assert_eq!(fs::read_to_string(&keygen_log)?, "");
    }
    Ok(())
}

#[test]
fn profile_config_rejects_wrong_typed_security_policy() -> TestResult {
    let repo = setup_repo("config-invalid-profile-policy")?;
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-invalid-profile-policy")
        .tempdir()?;
    let profile_config = fixture.path().join("profile-config.json");
    let before = fs::read(repo.path().join(".git/config"))?;

    for content in [
        r#"{"security":"not-an-object"}"#,
        r#"{"security":{"deploy_key":42}}"#,
    ] {
        fs::write(&profile_config, content)?;
        let mut command = wrix_command_with_path(repo.path(), &[])?;
        command
            .arg("--profile-config")
            .arg(&profile_config)
            .arg("init")
            .args(["--offline", "--no-sign"])
            .env("HOME", fixture.path().join("home"));
        let result = run_command(&mut command)?;
        assert_failure_with_clean_stdout(&result);
        assert_contains(
            "invalid profile policy",
            &result.stderr,
            "invalid profile config JSON",
        );
        assert_eq!(before, fs::read(repo.path().join(".git/config"))?);
    }
    Ok(())
}

fn init_command(
    repo: &Path,
    home: &Path,
    deploy_key: &Path,
    signing_key: &Path,
    hooks: &Path,
    extra_paths: &[&Path],
) -> TestResult<Command> {
    let mut command = wrix_command_with_path(repo, extra_paths)?;
    command
        .env("HOME", home)
        .env("HOSTNAME", "devhost")
        .env("WRIX_DEPLOY_KEY", deploy_key)
        .env("WRIX_SIGNING_KEY", signing_key)
        .env("WRIX_PREK_HOOKS", hooks)
        .env("WRIX_PREK_RUNNER", &common::prek::runtime()?.runner);
    Ok(command)
}

fn assert_policy_line(label: &str, output: &str, key: &str, value: &str) {
    assert_contains(label, output, &format!("{key}: {value}"));
}
