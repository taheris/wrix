mod common;

use std::{fs, path::Path, process::Command};

use common::{
    TestResult, assert_contains, assert_success_with_clean_stderr, run_command, run_git,
    setup_repo, write_ed25519_key, write_empty_key, write_online_success_git, write_prek_hooks,
    wrix_command_with_path,
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
    assert_policy_line("derived defaults", &result.stdout, "sign_commits", "true");
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
        r#"[wrix.git]
deploy_key = "toml-key"
sign_commits = false
remote = "upstream"

[wrix.init]
prek_hooks = false
online_verify = false
"#,
    )?;
    let mut command = init_command(repo.path(), &home, &deploy_key, &signing_key, &hooks, &[])?;
    command
        .arg("--profile-config")
        .arg(&profile_config)
        .arg("init");
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_policy_line("wrix.toml", &result.stdout, "deploy_key", "toml-key");
    assert_policy_line("wrix.toml", &result.stdout, "sign_commits", "false");
    assert_policy_line("wrix.toml", &result.stdout, "remote", "upstream");
    assert_policy_line("wrix.toml", &result.stdout, "prek_hooks", "false");
    assert_policy_line("wrix.toml", &result.stdout, "online_verify", "false");

    fs::write(
        repo.path().join("wrix.toml"),
        r#"[wrix.git]
deploy_key = "toml-key"
sign_commits = true
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
    assert_policy_line("flag overrides", &result.stdout, "sign_commits", "false");
    assert_policy_line("flag overrides", &result.stdout, "remote", "origin");
    assert_policy_line("flag overrides", &result.stdout, "prek_hooks", "false");
    assert_policy_line("flag overrides", &result.stdout, "online_verify", "false");
    assert_policy_line("flag overrides", &result.stdout, "force", "true");

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
        .env("WRIX_PREK_HOOKS", hooks);
    Ok(command)
}

fn assert_policy_line(label: &str, output: &str, key: &str, value: &str) {
    assert_contains(label, output, &format!("{key}: {value}"));
}
