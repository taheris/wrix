mod common;

use std::fs;

use common::{
    TestResult, assert_failure_with_clean_stdout, assert_success_with_clean_stderr, run_command,
    setup_repo, wrix_command,
};

#[test]
fn cache_config_reads_repository_policy_outside_direnv_without_mutation() -> TestResult {
    let repo = setup_repo("cache-config")?;
    let policy = "[cache]\nwarn_size = '2G'\n[cache.warm]\nchecks = true\n";
    fs::write(repo.path().join("wrix.toml"), policy)?;
    let nested = repo.path().join("src/nested");
    fs::create_dir_all(&nested)?;
    let before = fs::read(repo.path().join(".git/config"))?;
    let mut command = wrix_command(&nested)?;
    command.args(["service", "cache", "config"]);
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    let settings: wrix_cache::config::Settings = serde_json::from_str(&result.stdout)?;
    assert!(settings.warm.checks);
    assert_eq!(settings.soft_limit_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(fs::read_to_string(repo.path().join("wrix.toml"))?, policy);
    assert_eq!(fs::read(repo.path().join(".git/config"))?, before);
    assert!(!repo.path().join(".wrix").exists());
    Ok(())
}

#[test]
fn cache_config_explicit_environment_beats_repository_policy() -> TestResult {
    let repo = setup_repo("cache-config-env")?;
    fs::write(
        repo.path().join("wrix.toml"),
        "cache.warm.checks = true\ncache.warm.include_roots = ['.#custom']\n",
    )?;
    let mut command = wrix_command(repo.path())?;
    command
        .args(["service", "cache", "config"])
        .env("WRIX_CACHE_WARM_CHECKS", "0")
        .env("WRIX_CACHE_WARM_INCLUDE", "");
    let result = run_command(&mut command)?;
    assert_success_with_clean_stderr(&result);
    let settings: wrix_cache::config::Settings = serde_json::from_str(&result.stdout)?;
    assert!(!settings.warm.checks);
    assert_eq!(settings.warm.include_roots, Vec::<String>::new());
    Ok(())
}

#[test]
fn prefixed_policy_fails_before_init_mutates_repository() -> TestResult {
    let repo = setup_repo("prefixed-policy")?;
    let before = fs::read(repo.path().join(".git/config"))?;
    for policy in [
        "[wrix.git]\nsign = true\n",
        "[wrix.cache]\nwarn_size = '1G'\n",
        "git.sign = false\nwrix.git.sign = true\n",
    ] {
        fs::write(repo.path().join("wrix.toml"), policy)?;
        let mut command = wrix_command(repo.path())?;
        command.args(["init", "--offline", "--no-hooks"]);
        let result = run_command(&mut command)?;
        assert_failure_with_clean_stdout(&result);
        assert!(result.stderr.contains("unknown field `wrix`"));
        assert_eq!(fs::read(repo.path().join(".git/config"))?, before);
        assert_eq!(fs::read_to_string(repo.path().join("wrix.toml"))?, policy);
        assert!(!repo.path().join(".git/wrix").exists());
    }
    Ok(())
}
