use std::{collections::BTreeMap, fs};

use wrix_cache::config::Settings;
use wrix_core::{path::Workspace, repository_policy};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn omitted_cache_policy_uses_documented_defaults() -> TestResult {
    let settings = Settings::resolve(&repository_policy::parse("")?.cache, &BTreeMap::new())?;
    assert!(settings.enable);
    assert!(settings.require_trusted_nix);
    assert!(settings.publish.packages && settings.publish.checks && settings.publish.dev_shell);
    assert!(settings.warm.packages && settings.warm.dev_shell);
    assert!(!settings.warm.checks);
    assert_eq!(settings.publish.include_roots, Vec::<String>::new());
    assert_eq!(settings.publish.exclude_roots, Vec::<String>::new());
    assert_eq!(settings.warm.include_roots, Vec::<String>::new());
    assert_eq!(settings.warm.exclude_roots, Vec::<String>::new());
    assert_eq!(settings.soft_limit_bytes, 53_687_091_200);
    assert_eq!(settings.pending_retention_secs, 604_800);
    assert_eq!(settings.prune_interval_secs, 86_400);
    assert_eq!(settings.lock_timeout_ms, 30_000);
    assert_eq!(settings.explicit_lock_timeout_ms, 30_000);
    Ok(())
}

#[test]
fn repository_cache_policy_overrides_only_specified_fields() -> TestResult {
    let policy = repository_policy::parse(
        r#"
[cache]
enable = false
require_trusted_nix = false
warn_size = "1G"
pending_ttl = "2d"
prune_interval = "3h"
lock_timeout = "2s"
explicit_lock_timeout = "5s"
[cache.publish]
packages = false
checks = false
dev_shell = false
include_roots = [".#custom"]
exclude_roots = [".#skip"]
[cache.warm]
checks = true
"#,
    )?;
    let settings = Settings::resolve(&policy.cache, &BTreeMap::new())?;
    assert!(!settings.enable);
    assert!(!settings.require_trusted_nix);
    assert_eq!(settings.soft_limit_bytes, 1_073_741_824);
    assert_eq!(settings.pending_retention_secs, 172_800);
    assert_eq!(settings.prune_interval_secs, 10_800);
    assert_eq!(settings.lock_timeout_ms, 2000);
    assert_eq!(settings.explicit_lock_timeout_ms, 5000);
    assert!(!settings.publish.packages && !settings.publish.checks && !settings.publish.dev_shell);
    assert_eq!(settings.publish.include_roots, [".#custom"]);
    assert_eq!(settings.publish.exclude_roots, [".#skip"]);
    assert!(settings.warm.packages && settings.warm.checks && settings.warm.dev_shell);
    Ok(())
}

#[test]
fn environment_can_reset_repository_policy_to_builtin_values_and_empty_lists() -> TestResult {
    let policy = repository_policy::parse(
        r#"
[cache]
enable = false
require_trusted_nix = false
warn_size = "1G"
pending_ttl = "1s"
prune_interval = "1s"
lock_timeout = "1s"
[cache.publish]
packages = false
checks = false
dev_shell = false
include_roots = [".#custom"]
exclude_roots = [".#skip"]
[cache.warm]
packages = false
checks = true
dev_shell = false
include_roots = [".#custom"]
exclude_roots = [".#skip"]
"#,
    )?;
    let env = BTreeMap::from(
        [
            ("WRIX_NIX_CACHE_ENABLE", "1"),
            ("WRIX_NIX_CACHE_REQUIRE_TRUSTED", "yes"),
            ("WRIX_CACHE_PUBLISH_PACKAGES", "true"),
            ("WRIX_CACHE_PUBLISH_CHECKS", "1"),
            ("WRIX_CACHE_PUBLISH_DEVSHELL", "1"),
            ("WRIX_CACHE_PUBLISH_INCLUDE", ""),
            ("WRIX_CACHE_PUBLISH_EXCLUDE", ""),
            ("WRIX_CACHE_WARM_PACKAGES", "1"),
            ("WRIX_CACHE_WARM_CHECKS", "no"),
            ("WRIX_CACHE_WARM_DEVSHELL", "1"),
            ("WRIX_CACHE_WARM_INCLUDE", ""),
            ("WRIX_CACHE_WARM_EXCLUDE", ""),
            ("WRIX_CACHE_SOFT_LIMIT_BYTES", "53687091200"),
            ("WRIX_CACHE_PENDING_RETENTION_SECS", "604800"),
            ("WRIX_CACHE_PRUNE_INTERVAL_SECS", "86400"),
            ("WRIX_CACHE_LOCK_TIMEOUT_MS", "30000"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    assert_eq!(Settings::resolve(&policy.cache, &env)?, Settings::default());
    Ok(())
}

#[test]
fn invalid_environment_overrides_fail_instead_of_falling_back() -> TestResult {
    for (name, value) in [
        ("WRIX_NIX_CACHE_ENABLE", "maybe"),
        ("WRIX_CACHE_WARM_CHECKS", ""),
        ("WRIX_CACHE_SOFT_LIMIT_BYTES", "1G"),
        ("WRIX_CACHE_PENDING_RETENTION_SECS", "-1"),
        ("WRIX_CACHE_PRUNE_INTERVAL_SECS", "18446744073709551616"),
    ] {
        let env = BTreeMap::from([(name.to_owned(), value.to_owned())]);
        let error = Settings::resolve(&repository_policy::parse("")?.cache, &env).unwrap_err();
        assert!(error.to_string().contains(name));
    }
    Ok(())
}

#[test]
fn overflowing_lock_duration_fails_resolution() -> TestResult {
    let policy = repository_policy::parse("cache.lock_timeout = '18446744073709551615s'")?;
    let error = Settings::resolve(&policy.cache, &BTreeMap::new()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lock_timeout overflows milliseconds")
    );
    Ok(())
}

#[test]
fn cache_policy_reader_uses_nearest_repository_not_shared_loom_service_identity() -> TestResult {
    let root = tempfile::tempdir()?;
    let inner = root.path().join(".loom/beads/task");
    fs::create_dir_all(root.path().join(".git"))?;
    fs::create_dir_all(inner.join(".git"))?;
    fs::write(root.path().join("wrix.toml"), "cache.warm.checks = false")?;
    fs::write(inner.join("wrix.toml"), "cache.warm.checks = true")?;
    let settings = Settings::for_workspace(&Workspace::from_path(&inner)?)?;
    assert!(settings.warm.checks);
    Ok(())
}

#[test]
fn immutable_snapshot_roundtrips_complete_settings_without_repository_lookup() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("cache-config.json");
    let policy = repository_policy::parse("cache.warn_size = 0\ncache.pending_ttl = '1s'")?;
    let settings = Settings::resolve(&policy.cache, &BTreeMap::new())?;
    fs::write(&path, serde_json::to_vec(&settings)?)?;
    assert_eq!(Settings::read_snapshot(&path)?, settings);
    fs::write(&path, "{}")?;
    assert!(Settings::read_snapshot(&path).is_err());
    Ok(())
}
