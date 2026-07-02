use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
};

use wrix_core::path::Workspace;
use wrix_service::lifecycle::{CacheMode, DoltEndpoint, DoltTransport, Plan};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn workspace_identity_is_stable_and_collision_resistant() -> TestResult {
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

    Ok(())
}

#[test]
fn temp_cache_only_workspace_does_not_start_service() -> TestResult {
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

    Ok(())
}

#[test]
fn dolt_endpoint_transport_is_platform_specific() -> TestResult {
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
