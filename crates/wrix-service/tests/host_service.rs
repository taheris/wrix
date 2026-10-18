use std::{fs, path::PathBuf, process::Command, sync::Barrier, thread};

use tempfile::TempDir;
use wrix_core::path::Workspace;
use wrix_service::lifecycle::{CacheMode, Error, Paths, Plan};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> std::io::Result<Self> {
        Ok(Self(tempfile::tempdir()?))
    }

    fn workspace(&self, name: &str) -> std::io::Result<Workspace> {
        let root = self.0.path().join(name);
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join(".beads/dolt"))?;
        Workspace::from_service_path(root)
    }

    fn paths(&self, workspace: &Workspace) -> Paths {
        Paths::new(
            self.0.path().join("state").join(workspace.hash().as_str()),
            self.0.path().join("cache").join(workspace.hash().as_str()),
        )
    }

    fn plan(&self, workspace: Workspace) -> Result<Plan, Error> {
        let paths = self.paths(&workspace);
        Plan::for_workspace_with_paths(workspace, CacheMode::Disabled, paths)
    }
}

fn with_tcp_transport(test: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if std::env::var_os("WRIX_REGISTRATION_TEST_CHILD").is_some() {
        return Ok(false);
    }
    let output = Command::new(std::env::current_exe()?)
        .args([test, "--exact", "--nocapture"])
        .env("WRIX_REGISTRATION_TEST_CHILD", "1")
        .env("WRIX_DOLT_TRANSPORT", "tcp")
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(true)
}

fn write_metadata(plan: &Plan, value: &serde_json::Value) -> TestResult {
    fs::create_dir_all(plan.paths().state_root())?;
    fs::write(plan.paths().services_path(), serde_json::to_vec(value)?)?;
    Ok(())
}

#[test]
fn registration_rejects_another_workspace_before_layout_changes() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.workspace("first")?;
    let peer = fixture.workspace("peer")?;
    let plan = fixture.plan(first.clone()).unwrap();
    let mut metadata: serde_json::Value = serde_json::from_str(&plan.services_json()).unwrap();
    metadata["workspace_hash"] = peer.hash().as_str().into();
    write_metadata(&plan, &metadata)?;
    let before = fs::read(plan.paths().services_path()).unwrap();

    assert!(matches!(
        fixture.plan(first),
        Err(Error::WorkspaceMismatch { .. })
    ));
    assert!(matches!(
        plan.ensure_layout(),
        Err(Error::WorkspaceMismatch { .. })
    ));
    assert_eq!(fs::read(plan.paths().services_path()).unwrap(), before);
    assert!(!plan.workspace().canonical_path().join(".wrix").exists());
    assert!(
        !fixture
            .0
            .path()
            .join("state/service-registration.lock")
            .exists()
    );
    Ok(())
}

#[test]
fn registration_rejects_unscoped_persisted_port_leases() -> TestResult {
    if with_tcp_transport("registration_rejects_unscoped_persisted_port_leases")? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let workspace = fixture.workspace("workspace")?;
    let plan = fixture.plan(workspace.clone())?;
    let valid: serde_json::Value = serde_json::from_str(&plan.services_json())?;
    for (host, port) in [
        ("0.0.0.0", 23_000),
        ("127.0.0.2", 23_000),
        ("127.0.0.1", 21_000),
    ] {
        let mut metadata = valid.clone();
        metadata["endpoints"]["dolt_tcp"] = serde_json::json!({ "host": host, "port": port });
        write_metadata(&plan, &metadata)?;
        assert!(matches!(
            fixture.plan(workspace.clone()),
            Err(Error::InvalidPortLease { .. })
        ));
        assert!(matches!(
            plan.ensure_layout(),
            Err(Error::InvalidPortLease { .. })
        ));
    }
    assert!(!workspace.canonical_path().join(".wrix").exists());
    Ok(())
}

#[test]
fn stopped_peer_port_leases_are_not_reallocated() -> TestResult {
    if with_tcp_transport("stopped_peer_port_leases_are_not_reallocated")? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let workspace = fixture.workspace("workspace")?;
    let plan = fixture.plan(workspace.clone())?;
    let peer = fixture.workspace("peer")?;
    let peer_plan = fixture.plan(peer)?;
    let mut metadata: serde_json::Value = serde_json::from_str(&peer_plan.services_json())?;
    let reserved = plan.dolt_port().unwrap().get();
    metadata["endpoints"]["dolt_tcp"]["port"] = reserved.into();
    write_metadata(&peer_plan, &metadata)?;

    let replanned = fixture.plan(workspace)?;
    assert_ne!(replanned.dolt_port().unwrap().get(), reserved);
    assert!(
        matches!(plan.ensure_layout(), Err(Error::PortLeaseConflict { port }) if port == reserved)
    );
    assert_eq!(
        fs::read(peer_plan.paths().services_path())?,
        serde_json::to_vec(&metadata)?
    );
    assert!(!plan.paths().services_path().exists());
    Ok(())
}

#[test]
fn busy_port_fallback_is_persisted_and_reused() -> TestResult {
    if with_tcp_transport("busy_port_fallback_is_persisted_and_reused")? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let workspace = fixture.workspace("workspace")?;
    let preferred = fixture.plan(workspace.clone())?.dolt_port().unwrap().get();
    let listener = std::net::TcpListener::bind(("127.0.0.1", preferred))?;
    let plan = fixture.plan(workspace.clone())?;
    let fallback = plan.dolt_port().unwrap().get();
    assert_ne!(fallback, preferred);
    assert!((23_000..25_000).contains(&fallback));
    plan.ensure_layout()?;
    drop(listener);

    let resumed = fixture.plan(workspace)?;
    assert_eq!(resumed.dolt_port().unwrap().get(), fallback);
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(plan.paths().services_path())?)?;
    assert_eq!(metadata["endpoints"]["dolt_tcp"]["host"], "127.0.0.1");
    assert_eq!(metadata["endpoints"]["dolt_tcp"]["port"], fallback);
    Ok(())
}

#[test]
fn concurrent_registration_rejects_stale_conflicting_plans() -> TestResult {
    if with_tcp_transport("concurrent_registration_rejects_stale_conflicting_plans")? {
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let first = fixture.workspace("first")?;
    let first_plan = fixture.plan(first.clone())?;
    let preferred = first_plan.dolt_port().unwrap();
    let mut second = None;
    for index in 0..20_000 {
        let candidate = fixture.workspace(&format!("second-{index}"))?;
        if candidate.hash().port_offset(2_000) == first.hash().port_offset(2_000) {
            second = Some(candidate);
            break;
        }
    }
    let second = second.expect("find a deterministic port collision");
    let second_plan = fixture.plan(second.clone())?;
    assert_eq!(second_plan.dolt_port(), Some(preferred));
    let barrier = Barrier::new(2);
    let results = thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            first_plan.ensure_layout()
        });
        let second = scope.spawn(|| {
            barrier.wait();
            second_plan.ensure_layout()
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(Error::PortLeaseConflict { .. })))
            .count(),
        1
    );

    let first = fixture.plan(first)?;
    let second = fixture.plan(second)?;
    assert_ne!(first.dolt_port(), second.dolt_port());
    first.ensure_layout()?;
    second.ensure_layout()?;
    assert!(first.paths().services_path().exists());
    assert!(second.paths().services_path().exists());
    Ok(())
}

#[test]
fn partial_registration_is_not_overwritten_or_treated_as_absent() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.workspace("workspace")?;
    let plan = fixture.plan(workspace.clone())?;
    fs::create_dir_all(plan.paths().state_root())?;
    fs::write(plan.paths().services_path(), b"{\"workspace_hash\":")?;
    assert!(matches!(
        fixture.plan(workspace.clone()),
        Err(Error::PersistedServicesJson { .. })
    ));
    assert!(matches!(
        plan.ensure_layout(),
        Err(Error::PersistedServicesJson { .. })
    ));
    assert_eq!(
        fs::read(plan.paths().services_path())?,
        b"{\"workspace_hash\":"
    );

    fs::remove_file(plan.paths().services_path())?;
    fs::create_dir(plan.paths().services_path())?;
    assert!(matches!(
        fixture.plan(workspace),
        Err(Error::InvalidRegistrationPath { .. })
    ));
    assert!(matches!(
        plan.ensure_layout(),
        Err(Error::InvalidRegistrationPath { .. })
    ));
    assert!(plan.paths().services_path().is_dir());
    assert!(!plan.workspace().canonical_path().join(".wrix").exists());
    Ok(())
}

#[test]
fn malformed_peer_registration_is_not_treated_as_absent() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.workspace("workspace")?;
    let plan = fixture.plan(workspace.clone()).unwrap();
    let peer_path: PathBuf = fixture.0.path().join("state/peer/services.json");
    fs::create_dir_all(peer_path.parent().unwrap()).unwrap();
    fs::write(&peer_path, b"{\"workspace_hash\":").unwrap();

    assert!(matches!(
        fixture.plan(workspace),
        Err(Error::PersistedServicesJson { .. })
    ));
    assert!(matches!(
        plan.ensure_layout(),
        Err(Error::PersistedServicesJson { .. })
    ));
    assert_eq!(fs::read(peer_path).unwrap(), b"{\"workspace_hash\":");
    assert!(!plan.paths().services_path().exists());
    Ok(())
}
