use std::{env, fs, io, path::PathBuf, process::Command};

use wrix_core::path::Workspace;
use wrix_service::lifecycle::{CacheMode, Paths, Plan};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const VALID_PUBLIC_KEY: &str = "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n";

#[test]
fn state_layout_is_outside_workspace_and_respects_opt_out() -> TestResult {
    if rerun_with_temp_cache_enabled("state_layout_is_outside_workspace_and_respects_opt_out")? {
        return Ok(());
    }
    let fixture = workspace_tempdir("cache-state-")?;
    let workspace_root = fixture.path().join("workspace");
    fs::create_dir_all(workspace_root.join(".git"))?;
    let workspace = Workspace::from_service_path(&workspace_root)?;

    let enabled_paths = Paths::new(
        fixture.path().join("state-enabled"),
        fixture.path().join("cache-enabled"),
    );
    seed_valid_key(&enabled_paths)?;
    let enabled = Plan::for_workspace_with_paths(
        workspace.clone(),
        CacheMode::Enabled,
        enabled_paths.clone(),
    )?;
    enabled.ensure_layout()?;

    assert!(!enabled.paths().state_root().starts_with(&workspace_root));
    assert!(!enabled.paths().cache_root().starts_with(&workspace_root));
    assert!(enabled.paths().cache_lock_path().is_file());
    assert!(enabled.paths().cache_status_path().is_file());
    assert!(enabled.paths().gcroots_dir().is_dir());
    assert!(enabled.paths().pending_dir().is_dir());
    assert!(enabled.paths().cache_secret_path().is_file());
    assert!(enabled.paths().cache_public_path().is_file());
    assert!(enabled.paths().publish_roots_path().is_file());
    assert!(enabled.paths().services_path().is_file());
    assert!(
        enabled
            .paths()
            .cache_root()
            .join("nix-cache-info")
            .is_file()
    );
    assert!(enabled.paths().cache_root().join("nar").is_dir());
    assert!(enabled.paths().cache_root().join("log").is_dir());
    assert!(matches!(enabled.cache_port(), Some(21_000..=22_999)));

    let services = fs::read_to_string(enabled.paths().services_path())?;
    assert!(services.contains("\"cache_http\": { \"host\": \"127.0.0.1\""));
    assert!(services.contains("\"state_root\""));
    assert!(services.contains("\"cache_root\""));

    let persisted =
        Plan::for_workspace_with_paths(workspace.clone(), CacheMode::Enabled, enabled_paths)?;
    assert_eq!(persisted.cache_port(), enabled.cache_port());

    let disabled_paths = Paths::new(
        fixture.path().join("state-disabled"),
        fixture.path().join("cache-disabled"),
    );
    let disabled = Plan::for_workspace_with_paths(workspace, CacheMode::Disabled, disabled_paths)?;
    disabled.ensure_layout()?;

    assert_eq!(disabled.cache_port(), None);
    assert!(disabled.paths().state_root().is_dir());
    assert!(disabled.paths().services_path().is_file());
    assert!(!disabled.paths().cache_root().exists());
    assert!(!disabled.paths().cache_lock_path().exists());
    assert!(!disabled.paths().cache_status_path().exists());
    assert!(!disabled.paths().gcroots_dir().exists());
    assert!(!disabled.paths().pending_dir().exists());
    assert!(!disabled.paths().keys_dir().exists());
    assert!(!disabled.paths().publish_roots_path().exists());
    let disabled_services = fs::read_to_string(disabled.paths().services_path())?;
    assert!(disabled_services.contains("\"cache_http\": null"));

    Ok(())
}

fn rerun_with_temp_cache_enabled(test_name: &str) -> TestResult<bool> {
    const CHILD_MARKER: &str = "WRIX_SERVICE_TEMP_CACHE_TEST_CHILD";
    if env::var_os(CHILD_MARKER).is_some() {
        return Ok(false);
    }
    let output = Command::new(env::current_exe()?)
        .arg(test_name)
        .arg("--exact")
        .arg("--nocapture")
        .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
        .env(CHILD_MARKER, "1")
        .output()?;
    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(true)
}

fn seed_valid_key(paths: &Paths) -> io::Result<()> {
    fs::create_dir_all(paths.keys_dir())?;
    fs::write(paths.cache_secret_path(), b"secret\n")?;
    fs::write(paths.cache_public_path(), VALID_PUBLIC_KEY)
}

fn workspace_tempdir(prefix: &str) -> TestResult<tempfile::TempDir> {
    let root = workspace_root()?.join("target/wrix-service-tests");
    fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .map_err(Into::into)
}

fn workspace_root() -> TestResult<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir
        .parent()
        .ok_or_else(|| io::Error::other("wrix-service manifest has no parent"))?;
    let root = crates_dir
        .parent()
        .ok_or_else(|| io::Error::other("crates directory has no parent"))?;
    Ok(root.to_path_buf())
}
