mod common;

use std::{fs, path::Path, process::Command};

use common::{
    TestResult, assert_contains, assert_success_with_clean_stderr, common_git_dir, git_stdout,
    run_command, run_git, setup_committed_repo, write_empty_key, write_prek_hooks, wrix_command,
};

#[test]
fn prek_hooks() -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("wrix-init-prek-fixtures")
        .tempdir()?;
    let hooks = write_prek_hooks(&fixture.path().join("hooks"))?;
    let home = fixture.path().join("home");
    let deploy_key = fixture.path().join("deploy-key");
    write_empty_key(&deploy_key)?;

    let repo = setup_committed_repo("prek-enabled", true)?;
    let mut command = init_command(repo.path(), &home, &deploy_key, &hooks)?;
    command
        .arg("init")
        .args(["--offline", "--no-sign", "--key", "prek-key"]);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains("enabled init output", &result.stdout, "prek_hooks: true");
    assert_eq!(core_hooks_path(repo.path())?, hooks.display().to_string());
    assert_common_config_origin("enabled hooks origin", repo.path())?;

    let integration = repo.path().join(".loom/integration");
    fs::create_dir_all(repo.path().join(".loom"))?;
    run_git(
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
    )?;
    assert_eq!(core_hooks_path(&integration)?, hooks.display().to_string());
    assert_common_config_origin("linked hooks origin", &integration)?;

    let sentinel = "legacy-hooks";
    let flag_repo = setup_committed_repo("prek-disabled-flag", true)?;
    run_git(flag_repo.path(), &["config", "core.hooksPath", sentinel])?;
    let mut command = init_command(flag_repo.path(), &home, &deploy_key, &hooks)?;
    command
        .arg("init")
        .args(["--offline", "--no-sign", "--no-hooks", "--key", "prek-key"]);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains("flag disabled output", &result.stdout, "prek_hooks: false");
    assert_eq!(core_hooks_path(flag_repo.path())?, sentinel);

    let config_repo = setup_committed_repo("prek-disabled-config", true)?;
    run_git(config_repo.path(), &["config", "core.hooksPath", sentinel])?;
    fs::write(
        config_repo.path().join("wrix.toml"),
        "[wrix.init]\nprek_hooks = false\n",
    )?;
    let mut command = init_command(config_repo.path(), &home, &deploy_key, &hooks)?;
    command
        .arg("init")
        .args(["--offline", "--no-sign", "--key", "prek-key"]);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    assert_contains(
        "config disabled output",
        &result.stdout,
        "prek_hooks: false",
    );
    assert_eq!(core_hooks_path(config_repo.path())?, sentinel);

    Ok(())
}

fn init_command(repo: &Path, home: &Path, deploy_key: &Path, hooks: &Path) -> TestResult<Command> {
    let mut command = wrix_command(repo)?;
    command
        .env("HOME", home)
        .env("WRIX_DEPLOY_KEY", deploy_key)
        .env("WRIX_PREK_HOOKS", hooks);
    Ok(command)
}

fn core_hooks_path(repo: &Path) -> TestResult<String> {
    git_stdout(repo, &["config", "--get", "core.hooksPath"])
}

fn assert_common_config_origin(label: &str, repo: &Path) -> TestResult {
    let common_dir = common_git_dir(repo)?;
    let origin = git_stdout(
        repo,
        &["config", "--show-origin", "--get", "core.hooksPath"],
    )?;
    let common_config = format!("file:{}", common_dir.join("config").display());
    let local_config = "file:.git/config";
    assert!(
        origin.contains(&common_config) || origin.starts_with(local_config),
        "{label}: hooksPath was not read from common config {}: {origin}",
        common_dir.join("config").display(),
    );
    Ok(())
}
