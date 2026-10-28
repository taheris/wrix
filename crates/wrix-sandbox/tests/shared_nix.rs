mod common;

use common::{ChildSpec, ProfileFixture, TestResult};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use wrix_sandbox::command::Command as Launch;

fn fixture() -> TestResult<(tempfile::TempDir, PathBuf, PathBuf)> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("checkout");
    fs::create_dir(&workspace)?;
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&workspace)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()?
            .success()
    );
    let profile = root.path().join("profile.json");
    common::write_profile_config(&profile, &ProfileFixture::default())?;
    Ok((root, workspace, profile))
}

fn invoke(
    root: &Path,
    workspace: &Path,
    profile: &Path,
    label: &str,
    mode: Option<&str>,
    preview: bool,
    spawn: bool,
) -> TestResult<common::ChildRun> {
    let mut args = if spawn {
        let config = root.join("spawn.json");
        let mut value = json!({"workspace":workspace,"env":[],"agent_args":["true"],"mounts":[]});
        if let Some(mode) = mode {
            value["nix_store"] = json!({"mode":mode});
        }
        fs::write(&config, serde_json::to_vec(&value)?)?;
        vec!["--spawn-config".into(), config.display().to_string()]
    } else {
        let mut args = Vec::new();
        if let Some(mode) = mode {
            args.extend(["--nix-store".into(), mode.into()]);
        }
        args
    };
    if preview {
        args.extend(["--dry-run".into(), "--json".into()]);
    }
    if !spawn {
        args.extend([workspace.display().to_string(), "true".into()]);
    }
    common::run_child(
        "store_child",
        root,
        label,
        ChildSpec {
            command: if spawn { Launch::Spawn } else { Launch::Run },
            profile_config: Some(profile.to_owned()),
            args,
            env: Vec::new(),
            capture_runtime: !preview,
        },
    )
}

#[test]
fn store_preflight_reports_mode_without_preparing_or_starting_shared_work() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    for spawn in [false, true] {
        let private = invoke(
            root.path(),
            &workspace,
            &profile,
            "private",
            None,
            true,
            spawn,
        )?;
        assert!(private.success, "{}", private.stderr);
        let value: Value = serde_json::from_str(&private.stdout)?;
        assert_eq!(value["plan"]["nix_store"]["mode"], "private");
        assert_eq!(value["plan"]["nix_store"]["approval"], "not_requested");
        let shared = invoke(
            root.path(),
            &workspace,
            &profile,
            "shared",
            Some("shared"),
            true,
            spawn,
        )?;
        assert!(!shared.success);
        let value: Value = serde_json::from_str(&shared.stdout)?;
        assert_eq!(value["plan"]["nix_store"]["mode"], "shared");
        assert_eq!(
            value["plan"]["nix_store"]["native_acceptance"],
            if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                "linux_podman"
            } else {
                "unverified"
            }
        );
        let blockers = value["plan"]["nix_store"]["blockers"].as_array().unwrap();
        assert!(blockers.contains(&json!("incompatible_image")));
        assert_eq!(
            blockers.contains(&json!("runtime_unavailable")),
            !cfg!(all(target_os = "linux", target_arch = "x86_64"))
        );
        assert!(
            value["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding["code"] == "nix_store_admission")
        );
    }
    assert!(!workspace.join(".wrix").exists());
    assert!(!root.path().join("argv").exists());
    Ok(())
}

#[test]
fn unproved_shared_launch_fails_before_service_runtime_staging_or_journal() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    let mut value: Value = serde_json::from_slice(&fs::read(&profile)?)?;
    value["services"]["nix_cache"]["enable"] = json!(true);
    fs::write(&profile, serde_json::to_vec(&value)?)?;
    for spawn in [false, true] {
        let result = invoke(
            root.path(),
            &workspace,
            &profile,
            "blocked",
            Some("shared"),
            false,
            spawn,
        )?;
        assert!(!result.success);
        let message = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            "Shared installed Nix stores require a compatible versioned image and seed closure"
        } else {
            "Shared installed Nix execution is unavailable"
        };
        assert!(result.stderr.contains(message), "{}", result.stderr);
        assert!(!root.path().join("argv").exists());
        assert!(!workspace.join(".wrix").exists());
    }
    Ok(())
}

#[test]
fn invocation_overrides_store_policy_but_never_skips_strict_policy_parsing() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    fs::write(
        workspace.join("wrix.toml"),
        "[nix_store]\nmode = 'shared'\n",
    )?;
    let original = fs::read(workspace.join("wrix.toml"))?;
    for spawn in [false, true] {
        assert!(
            !invoke(
                root.path(),
                &workspace,
                &profile,
                "inherited",
                None,
                true,
                spawn
            )?
            .success
        );
        let result = invoke(
            root.path(),
            &workspace,
            &profile,
            "override",
            Some("private"),
            true,
            spawn,
        )?;
        assert!(result.success, "{}", result.stderr);
        assert_eq!(
            serde_json::from_str::<Value>(&result.stdout)?["plan"]["nix_store"]["mode"],
            "private"
        );
    }
    assert_eq!(fs::read(workspace.join("wrix.toml"))?, original);
    for invalid in [
        "[nix_store]\nmode = 'PRIVATE'\n",
        "[nix_store]\nmode = 'shared'\napproved = true\n",
        "[nix_store]\npool = 'peer'\n",
    ] {
        fs::write(workspace.join("wrix.toml"), invalid)?;
        for spawn in [false, true] {
            let result = invoke(
                root.path(),
                &workspace,
                &profile,
                "invalid",
                Some("private"),
                false,
                spawn,
            )?;
            assert!(!result.success);
            assert!(
                result.stderr.contains("invalid repository policy"),
                "{}",
                result.stderr
            );
            assert!(!root.path().join("argv").exists());
        }
    }
    Ok(())
}

#[test]
fn explicit_private_launch_overrides_shared_repository_policy() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    fs::write(
        workspace.join("wrix.toml"),
        "[nix_store]\nmode = 'shared'\n",
    )?;
    for spawn in [false, true] {
        let result = invoke(
            root.path(),
            &workspace,
            &profile,
            "private-launch",
            Some("private"),
            false,
            spawn,
        )?;
        assert!(result.success, "{}", result.stderr);
        assert!(root.path().join("argv").exists());
    }
    Ok(())
}

#[test]
fn shared_store_under_limit_is_blocked_even_with_capability_metadata() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    let mut value: Value = serde_json::from_slice(&fs::read(&profile)?)?;
    value["network"]["default_mode"] = json!("limit");
    value["image"]["shared_nix"] = json!({"version":1,"seed_closure":"/nix/store/00000000000000000000000000000000-seed/store-paths"});
    fs::write(&profile, serde_json::to_vec(&value)?)?;
    for spawn in [false, true] {
        let result = invoke(
            root.path(),
            &workspace,
            &profile,
            "restricted",
            Some("shared"),
            true,
            spawn,
        )?;
        assert!(!result.success);
        let value: Value = serde_json::from_str(&result.stdout)?;
        assert!(
            value["plan"]["nix_store"]["blockers"]
                .as_array()
                .unwrap()
                .contains(&json!("restricted_network")),
            "{}",
            result.stdout
        );
    }
    Ok(())
}

#[test]
fn spawn_store_object_rejects_authority_and_invalid_owned_values_before_effects() -> TestResult {
    let (root, workspace, profile) = fixture()?;
    for invalid in [
        json!(null),
        json!("shared"),
        json!({"mode":null}),
        json!({"mode":false}),
        json!({"mode":"SHARED"}),
        json!({"mode":"private", "pool":"peer"}),
        json!({"mode":"shared", "approved":true}),
        json!({"endpoint":"unix:///peer"}),
    ] {
        let config = root.path().join("invalid-spawn.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({"workspace":workspace, "nix_store":invalid}))?,
        )?;
        let result = common::run_child(
            "store_child",
            root.path(),
            "invalid-spawn",
            ChildSpec {
                command: Launch::Spawn,
                profile_config: Some(profile.clone()),
                args: vec!["--spawn-config".into(), config.display().to_string()],
                env: Vec::new(),
                capture_runtime: true,
            },
        )?;
        assert!(!result.success);
        assert!(!root.path().join("argv").exists());
        assert!(!workspace.join(".wrix").exists());
    }
    Ok(())
}

#[test]
#[ignore = "child process receives per-test environment"]
fn store_child() -> TestResult {
    common::run_command_child()
}
