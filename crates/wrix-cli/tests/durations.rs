mod common;

use std::fs;

use common::{
    TestResult, assert_failure_with_clean_stdout, assert_success_with_clean_stderr, run_command,
    setup_repo, wrix_command,
};
use wrix_core::duration;

#[test]
fn human_duration_inputs_use_humantime() -> TestResult {
    let repo = setup_repo("human-durations")?;
    let git_before = fs::read(repo.path().join(".git/config"))?;
    let policy = repo.path().join("wrix.toml");
    fs::write(
        &policy,
        "cache.pending_ttl = '1week 2days'\ncache.lock_timeout = '1.5s'",
    )?;
    let mut command = wrix_command(repo.path())?;
    command.args(["service", "cache", "config"]);
    let output = run_command(&mut command)?;
    assert_success_with_clean_stderr(&output);
    let settings: wrix_cache::config::Settings = serde_json::from_str(&output.stdout)?;
    assert_eq!(settings.pending_retention_secs, 777_600);
    assert_eq!(settings.lock_timeout_ms, 1500);
    for input in ["forever", "1us", "18446744073709552s"] {
        let content = format!("cache.lock_timeout = '{input}'");
        fs::write(&policy, &content)?;
        let output = run_command(&mut command)?;
        assert_failure_with_clean_stdout(&output);
        assert_eq!(fs::read_to_string(&policy)?, content);
    }
    assert_eq!(fs::read(repo.path().join(".git/config"))?, git_before);
    assert!(!repo.path().join(".wrix").exists());
    Ok(())
}

#[test]
fn wait_and_retention_domains_keep_distinct_zero_precision_and_range_rules() {
    for (input, expected) in [("2minutes 500ms", 120_500), ("1.5s", 1500), ("0s", 0)] {
        assert_eq!(duration::milliseconds(input).unwrap(), expected);
    }
    assert_eq!(duration::retention_seconds("1week 2days").unwrap(), 777_600);
    assert_eq!(
        duration::retention_seconds("2147483647s").unwrap(),
        i32::MAX
    );
    for input in ["", "-1s", "forever", "1G", "18446744073709551616s"] {
        assert!(duration::milliseconds(input).is_err(), "{input}");
        assert!(duration::retention_seconds(input).is_err(), "{input}");
    }
    for input in ["0s", "1.5s", "2147483648s"] {
        assert!(duration::retention_seconds(input).is_err(), "{input}");
    }
    for input in ["1us", "18446744073709552s"] {
        assert!(duration::milliseconds(input).is_err(), "{input}");
    }
}
