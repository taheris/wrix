use super::*;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn reserve(range: std::ops::Range<u16>) -> TcpListener {
    for port in range {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => return listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => (),
            Err(error) => panic!("fixture listener: {error}"),
        }
    }
    panic!("fixture port range exhausted")
}

struct Fixture {
    root: tempfile::TempDir,
    authority: PathBuf,
    ports: [NonZeroU16; 5],
    cache: NonZeroU16,
    reservations: Vec<TcpListener>,
    _ports: File,
}
impl Fixture {
    fn new() -> Self {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(format!(
                "/tmp/wrix-endpoint-tests-{}.lock",
                rustix::process::geteuid().as_raw()
            ))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let root = tempfile::tempdir().unwrap();
        let authority = root.path().join("control/authority");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&authority)
            .unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join("host-lifecycle"))
            .unwrap();
        let reservations = vec![
            reserve(24_500..25_000),
            reserve(22_500..23_000),
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            reserve(24_000..24_500),
            reserve(22_000..22_500),
        ];
        let ports = std::array::from_fn(|index| {
            NonZeroU16::new(reservations[index].local_addr().unwrap().port()).unwrap()
        });
        let cache = NonZeroU16::new(reservations[5].local_addr().unwrap().port()).unwrap();
        private_file(
            &root.path().join("host-lifecycle/celler-owner.json"),
            &serde_json::to_vec(&serde_json::json!({
                "schema_version":3, "backend_version":"celler-v2", "runtime":"podman",
                "container_name":"shared", "cache_http_port":cache
            }))
            .unwrap(),
        );
        private_file(&authority.join("ownership"), b"");
        Self {
            root,
            authority,
            ports,
            cache,
            reservations,
            _ports: lock,
        }
    }
    fn prior(&self) -> Vec<u8> {
        let bytes = serde_json::to_vec(&Snapshot {
            version: 2,
            ports: self.ports.to_vec(),
        })
        .unwrap();
        private_file(&self.authority.join("role-endpoints.json"), &bytes);
        bytes
    }
}

#[tokio::test]
async fn first_bind_adopts_retained_cache_sql_and_sync_endpoints() -> TestResult {
    let mut fixture = Fixture::new();
    private_file(
        &fixture.root.path().join("host-lifecycle/shared.json"),
        &serde_json::to_vec(&serde_json::json!({
            "version":1,"tcp":true,"sql_port":fixture.ports[0],"sync_port":fixture.ports[4]
        }))?,
    );
    fixture.reservations.clear();
    let store = Store::open(&fixture.authority)?;
    let listeners = store.bind().await?;
    assert_eq!(
        listeners.address(Role::CacheRead)?.port(),
        fixture.cache.get()
    );
    assert_eq!(listeners.address(Role::Sql)?.port(), fixture.ports[0].get());
    assert_eq!(
        listeners.address(Role::Sync)?.port(),
        fixture.ports[4].get()
    );
    assert_eq!(store.bound_ports()?.unwrap()[1], fixture.cache);
    Ok(())
}

#[tokio::test]
async fn occupied_retained_port_is_not_relocated_or_published() -> TestResult {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.authority)?;
    let Err(error) = store.bind().await else {
        panic!("relocated occupied retained port")
    };
    assert!(
        matches!(error, Error::Route(crate::route::Error::RetainedPort { role:Role::CacheRead, port, source }) if port == fixture.cache && source.kind() == io::ErrorKind::AddrInUse)
    );
    assert!(!fixture.authority.join("role-endpoints.json").exists());
    Ok(())
}

#[tokio::test]
async fn issued_ports_report_physical_listeners_not_new_retention_metadata() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.reservations.clear();
    let store = Store::open(&fixture.authority)?;
    let _listeners = store.bind().await?;
    let owner = fixture.root.path().join("host-lifecycle/celler-owner.json");
    let mut changed: serde_json::Value = serde_json::from_slice(&fs::read(&owner)?)?;
    changed["cache_http_port"] = serde_json::json!(fixture.ports[1]);
    private_file(&owner, &serde_json::to_vec(&changed)?);
    assert_eq!(store.bound_ports()?.unwrap()[1], fixture.cache);
    assert!(matches!(
        Store::open(&fixture.authority),
        Err(Error::RetainedEndpoints { .. })
    ));
    Ok(())
}

#[test]
fn conflicting_snapshot_is_rejected_during_ordinary_open_without_rewriting() {
    let fixture = Fixture::new();
    let bytes = fixture.prior();
    assert!(
        matches!(Store::open(&fixture.authority), Err(Error::RetainedEndpoints { role:Role::CacheRead, retained, recorded }) if retained == fixture.cache && recorded == fixture.ports[1])
    );
    assert_eq!(
        fs::read(fixture.authority.join("role-endpoints.json")).unwrap(),
        bytes
    );
}

#[test]
fn repair_preserves_prior_leases_native_data_and_original_ownership_inode() -> TestResult {
    let mut fixture = Fixture::new();
    let prior = fixture.prior();
    let native = fixture.root.path().join("native-data");
    private_file(&native, b"database and signing state");
    private_file(
        &fixture.root.path().join("host-lifecycle/update.json"),
        b"exact intent",
    );
    let inode = fs::metadata(fixture.authority.join("ownership"))?.ino();
    fixture.reservations.clear();
    assert!(needs_repair(fixture.root.path())?);
    repair(fixture.root.path(), &"a".repeat(32))?;
    assert!(!needs_repair(fixture.root.path())?);
    let mut expected = fixture.ports;
    expected[1] = fixture.cache;
    let store = Store::open(&fixture.authority)?;
    assert_eq!(store.read_snapshot()?, expected.map(Some));
    assert_eq!(fs::read(native)?, b"database and signing state");
    assert_eq!(
        fs::read(fixture.root.path().join("host-lifecycle/update.json"))?,
        b"exact intent"
    );
    assert_eq!(
        fs::metadata(fixture.authority.join("ownership"))?.ino(),
        inode
    );
    let archives = fs::read_dir(&fixture.authority)?
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("role-endpoints-before-")
        })
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(archives[0].path())?, prior);
    Ok(())
}

#[test]
fn active_native_controller_ownership_refuses_repair_without_rewriting() -> TestResult {
    let mut fixture = Fixture::new();
    let prior = fixture.prior();
    fixture.reservations.clear();
    let _controller = crate::control::registration::Ownership::acquire(&fixture.authority)?;
    let error = repair(fixture.root.path(), &"a".repeat(32)).unwrap_err();
    assert!(matches!(error, Error::EndpointOwnershipBusy));
    assert_eq!(
        fs::read(fixture.authority.join("role-endpoints.json"))?,
        prior
    );
    Ok(())
}

#[test]
fn occupied_retained_port_refuses_repair_without_rewriting() -> TestResult {
    let fixture = Fixture::new();
    let prior = fixture.prior();
    assert!(repair(fixture.root.path(), &"a".repeat(32)).is_err());
    assert_eq!(
        fs::read(fixture.authority.join("role-endpoints.json"))?,
        prior
    );
    Ok(())
}

#[test]
fn interrupted_archival_retries_without_discarding_original_evidence() -> TestResult {
    let mut fixture = Fixture::new();
    let prior = fixture.prior();
    archive(&fixture.authority, &"a".repeat(32))?;
    fixture.reservations.clear();
    repair(fixture.root.path(), &"a".repeat(32))?;
    let corrected = fs::read(fixture.authority.join("role-endpoints.json"))?;
    repair(fixture.root.path(), &"a".repeat(32))?;
    assert_eq!(
        fs::read(fixture.authority.join("role-endpoints.json"))?,
        corrected
    );
    assert_eq!(
        fs::read(fixture.authority.join(format!(
            "role-endpoints-before-{}-{:x}.json",
            "a".repeat(32),
            Sha256::digest(&prior)
        )))?,
        prior
    );
    Ok(())
}
