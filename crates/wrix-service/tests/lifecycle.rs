use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
};

use wrix_core::path::{Workspace, WorkspaceHash};
use wrix_service::lifecycle::{CacheMode, DoltEndpoint, DoltTransport, Paths, Plan};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn workspace_identity_is_stable_and_collision_resistant() -> TestResult {
    if rerun_with_temp_cache_enabled("workspace_identity_is_stable_and_collision_resistant")? {
        return Ok(());
    }
    let root = repo_with_dolt("identity-primary")?;
    let child = root.join("nested/workspace");
    fs::create_dir_all(&child)?;

    let plan = plan_for_service_path(&root, CacheMode::Enabled)?;
    let same_identity = plan_for_service_path(&child, CacheMode::Enabled)?;
    let other = distinct_plan(&plan)?;

    assert_eq!(plan.workspace().canonical_path(), root.canonicalize()?);
    assert_eq!(plan.workspace().hash(), same_identity.workspace().hash());
    assert_eq!(plan.container_name(), same_identity.container_name());
    assert_eq!(plan.cache_port(), same_identity.cache_port());
    assert_eq!(
        plan.paths().state_root(),
        same_identity.paths().state_root()
    );
    assert_eq!(
        plan.paths().cache_root(),
        same_identity.paths().cache_root()
    );
    assert_eq!(dolt_endpoint(&plan)?, dolt_endpoint(&same_identity)?);

    assert_ne!(plan.workspace().hash(), other.workspace().hash());
    assert_ne!(plan.container_name(), other.container_name());
    assert_ne!(plan.cache_port(), other.cache_port());
    assert_ne!(plan.paths().state_root(), other.paths().state_root());
    assert_ne!(plan.paths().cache_root(), other.paths().cache_root());
    assert_ne!(
        dolt_endpoint(&plan)?.socket_path(),
        dolt_endpoint(&other)?.socket_path()
    );
    assert_cache_command_uses_workspace(&root, plan.workspace().hash())?;
    assert_cache_command_uses_workspace(&child, plan.workspace().hash())?;
    assert_cache_command_uses_workspace(
        other.workspace().canonical_path(),
        other.workspace().hash(),
    )?;

    Ok(())
}

#[test]
fn legacy_workspace_hash_metadata_uses_collision_safe_plan() -> TestResult {
    let root = repo("legacy-workspace-hash")?;
    let workspace = Workspace::from_service_path(&root)?;
    let fixture = tempfile::Builder::new()
        .prefix("legacy-workspace-hash")
        .tempdir()?;
    let state_base = fixture.path().join("state");
    let legacy_state = state_base.join("0123456789abcdef");
    let paths = Paths::new(
        state_base.join(workspace.hash().as_str()),
        fixture.path().join("cache"),
    );
    fs::create_dir_all(&legacy_state)?;
    fs::write(
        legacy_state.join("services.json"),
        format!(
            concat!(
                "{{\n",
                "  \"workspace_hash\": \"0123456789abcdef\",\n",
                "  \"container_name\": \"{}\",\n",
                "  \"endpoints\": {{ \"cache_http\": null, \"dolt_tcp\": null }}\n",
                "}}\n"
            ),
            workspace.container_name()
        ),
    )?;

    let expected_name = workspace.disambiguated_container_name();
    let plan = Plan::for_workspace_with_paths(workspace, CacheMode::Disabled, paths)?;

    assert_eq!(plan.container_name(), expected_name);
    Ok(())
}

#[test]
fn temp_cache_only_plan_has_no_services() -> TestResult {
    let root = temp_workspace("temp-cache-only")?;
    let workspace = Workspace::from_service_path(&root)?;
    let plan = Plan::for_workspace(workspace, CacheMode::Enabled)?;
    let services = plan.services_json();

    assert_eq!(plan.cache_port(), None);
    assert!(plan.dolt().is_none());
    assert!(services.contains("\"cache_http\": null"));
    assert!(services.contains("\"dolt\": null"));

    Ok(())
}

#[test]
fn loom_bead_workspace_uses_repo_service_identity() -> TestResult {
    let root = repo("loom-bead-repo")?;
    let bead = root.join(".loom/beads/wx-cy0iz.19");
    fs::create_dir_all(bead.join(".git"))?;

    let repo_plan = plan_for_service_path(&root, CacheMode::Disabled)?;
    let bead_plan = plan_for_service_path(&bead, CacheMode::Disabled)?;
    let raw_bead = Plan::for_workspace(Workspace::from_path(&bead)?, CacheMode::Disabled)?;

    assert_eq!(
        bead_plan.workspace().canonical_path(),
        repo_plan.workspace().canonical_path()
    );
    assert_eq!(bead_plan.workspace().hash(), repo_plan.workspace().hash());
    assert_eq!(bead_plan.container_name(), repo_plan.container_name());
    assert_ne!(raw_bead.workspace().hash(), repo_plan.workspace().hash());
    assert_ne!(raw_bead.container_name(), repo_plan.container_name());
    assert_cache_command_uses_workspace(&bead, repo_plan.workspace().hash())?;

    Ok(())
}

#[test]
fn loom_integration_workspace_uses_repo_service_identity() -> TestResult {
    let root = repo("loom-integration-repo")?;
    let integration = root.join(".loom/integration");
    fs::create_dir_all(&integration)?;

    let repo_plan = plan_for_service_path(&root, CacheMode::Disabled)?;
    let integration_plan = plan_for_service_path(&integration, CacheMode::Disabled)?;
    let raw_integration =
        Plan::for_workspace(Workspace::from_path(&integration)?, CacheMode::Disabled)?;

    assert_eq!(
        integration_plan.workspace().canonical_path(),
        repo_plan.workspace().canonical_path()
    );
    assert_eq!(
        integration_plan.workspace().hash(),
        repo_plan.workspace().hash()
    );
    assert_eq!(
        integration_plan.container_name(),
        repo_plan.container_name()
    );
    assert_ne!(
        raw_integration.workspace().hash(),
        repo_plan.workspace().hash()
    );
    assert_ne!(raw_integration.container_name(), repo_plan.container_name());
    assert_cache_command_uses_workspace(&integration, repo_plan.workspace().hash())?;

    Ok(())
}

#[test]
fn default_dolt_transport_matches_current_platform() -> TestResult {
    let root = repo_with_dolt("dolt-transport")?;
    let plan = plan_for_service_path(&root, CacheMode::Disabled)?;
    let endpoint = dolt_endpoint(&plan)?;
    let services = plan.services_json();

    if cfg!(target_os = "macos") {
        assert_eq!(endpoint.transport(), DoltTransport::Tcp);
        assert_eq!(endpoint.tcp_host(), Some("127.0.0.1"));
        assert!(matches!(endpoint.tcp_port(), Some(23_000..=24_999)));
        assert!(services.contains("\"transport\": \"tcp\""));
        assert!(services.contains("BEADS_DOLT_SERVER_HOST"));
        assert!(services.contains("BEADS_DOLT_SERVER_PORT"));
    } else {
        assert_eq!(endpoint.transport(), DoltTransport::UnixSocket);
        assert_eq!(endpoint.tcp_host(), None);
        assert_eq!(endpoint.tcp_port(), None);
        assert_eq!(endpoint.socket_path(), root.join(".wrix/dolt.sock"));
        assert!(services.contains("\"transport\": \"unix\""));
        assert!(services.contains("BEADS_DOLT_SERVER_SOCKET"));
    }

    Ok(())
}

fn rerun_with_temp_cache_enabled(test_name: &str) -> TestResult<bool> {
    const CHILD_MARKER: &str = "WRIX_SERVICE_TEMP_CACHE_TEST_CHILD";
    if env::var_os(CHILD_MARKER).is_some() {
        return Ok(false);
    }
    let fixture = tempfile::Builder::new()
        .prefix("service-temp-cache-child")
        .tempdir()?;
    let home = fixture.path().join("home");
    fs::create_dir_all(&home)?;
    let output = Command::new(env::current_exe()?)
        .arg(test_name)
        .arg("--exact")
        .arg("--nocapture")
        .env("HOME", home)
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .env("XDG_CACHE_HOME", fixture.path().join("cache"))
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

#[test]
#[ignore = "child process receives an isolated current directory and cache roots"]
fn cache_identity_child() -> TestResult {
    let args = ["service", "cache", "status"].map(String::from);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = wrix_cli::command::run(&args, &mut stdout, &mut stderr)?;

    assert_eq!(status, std::process::ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert!(String::from_utf8(stdout)?.contains("cache size:"));
    Ok(())
}

fn assert_cache_command_uses_workspace(path: &Path, expected: &WorkspaceHash) -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("cache-command-identity")
        .tempdir()?;
    let home = fixture.path().join("home");
    let state_home = fixture.path().join("state");
    let cache_home = fixture.path().join("cache");
    let state_root = state_home.join("wrix/workspaces").join(expected.as_str());
    fs::create_dir_all(&home)?;
    fs::create_dir_all(state_root.join("keys"))?;
    fs::write(state_root.join("keys/cache.secret"), "secret\n")?;
    fs::write(
        state_root.join("keys/cache.pub"),
        "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    )?;

    let mut command = Command::new(std::env::current_exe()?);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("WRIX_") {
            command.env_remove(name);
        }
    }
    let output = command
        .arg("cache_identity_child")
        .arg("--exact")
        .arg("--ignored")
        .current_dir(path)
        .env("HOME", home)
        .env("XDG_STATE_HOME", &state_home)
        .env("XDG_CACHE_HOME", &cache_home)
        .env("WRIX_NIX_STORE_BIN", "/bin/false")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "cache identity command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    let workspace_dirs = fs::read_dir(state_home.join("wrix/workspaces"))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    assert_eq!(workspace_dirs, vec![expected.as_str()]);
    Ok(())
}

fn distinct_plan(first: &Plan) -> TestResult<Plan> {
    for index in 0..128 {
        let root = repo_with_dolt(&format!("identity-other-{index}"))?;
        let plan = plan_for_service_path(root, CacheMode::Enabled)?;
        if plan.cache_port() != first.cache_port() {
            return Ok(plan);
        }
    }
    Err(io::Error::other("could not find a distinct service-port fixture").into())
}

fn dolt_endpoint(plan: &Plan) -> TestResult<&DoltEndpoint> {
    plan.dolt()
        .ok_or_else(|| io::Error::other("expected Dolt endpoint").into())
}

fn plan_for_service_path(path: impl Into<PathBuf>, cache_mode: CacheMode) -> TestResult<Plan> {
    let workspace = Workspace::from_service_path(path.into())?;
    Ok(Plan::for_workspace(workspace, cache_mode)?)
}

fn repo_with_dolt(name: &str) -> TestResult<PathBuf> {
    let root = repo(name)?;
    fs::create_dir_all(root.join(".beads/dolt"))?;
    Ok(root)
}

fn repo(name: &str) -> TestResult<PathBuf> {
    let root = target_workspace(name)?;
    fs::create_dir_all(root.join(".git"))?;
    Ok(root)
}

fn target_workspace(name: &str) -> TestResult<PathBuf> {
    let root = workspace_root()?
        .join("target/wrix-service-tests/lifecycle")
        .join(format!("{name}-{}", std::process::id()));
    reset_dir(&root)?;
    Ok(root)
}

fn temp_workspace(name: &str) -> TestResult<PathBuf> {
    let root = std::env::temp_dir().join(format!("wrix-service-{name}-{}", std::process::id()));
    reset_dir(&root)?;
    Ok(root)
}

fn reset_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(path)
}

fn workspace_root() -> TestResult<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir
        .parent()
        .ok_or_else(|| io::Error::other("wrix-service manifest has no parent"))?;
    let root = crates_dir
        .parent()
        .ok_or_else(|| io::Error::other("crates directory has no parent"))?;
    Ok(non_loom_root(root).unwrap_or_else(|| root.to_path_buf()))
}

fn non_loom_root(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    components
        .iter()
        .position(|component| component.as_os_str() == OsStr::new(".loom"))
        .map(|index| path_from_components(&components[..index]))
        .filter(|root| !root.as_os_str().is_empty())
}

fn path_from_components(components: &[Component<'_>]) -> PathBuf {
    let mut path = PathBuf::new();
    for component in components {
        path.push(component.as_os_str());
    }
    path
}
