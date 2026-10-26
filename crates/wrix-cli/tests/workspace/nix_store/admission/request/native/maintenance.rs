//! Actual pool exclusion, clean drain, interrupted host intent and same-software native resume.
use super::*;
use wrix_service::{
    control::supervision::Package,
    nix_store::{
        lifecycle::{Error, Repair},
        owner::backend,
    },
};
fn package() -> Package {
    let package = Package::load(Path::new(
        &std::env::var_os("WRIX_TEST_POOL_CONTROLLER_BINDING").unwrap(),
    ))
    .unwrap();
    assert_eq!(
        package.executable(),
        PathBuf::from(std::env::var_os("WRIX_TEST_POOL_CONTROLLER").unwrap())
    );
    package
}
pub(super) fn busy(host: &wrix_state::host::Host) {
    assert!(matches!(
        Repair::begin(host, &package()),
        Err(Error::Backend(backend::Error::Busy) | Error::Busy)
    ));
    assert!(
        !host
            .roots()
            .state()
            .join("nix-pool-maintenance.json")
            .exists()
    );
}
fn fenced(runtime: &tokio::runtime::Runtime, client: &Client, selection: &Selection) {
    let id = runtime
        .block_on(client.prepare_nix_pool(selection, system()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match runtime
            .block_on(client.request(Operation::InspectNixPoolPreparation { id: id.clone() }))
            .unwrap()
        {
            Reply::PoolPreparation {
                status: PoolPreparation::Failed,
            } => break,
            Reply::PoolPreparation {
                status: PoolPreparation::Pending,
            } => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(25));
            }
            reply => panic!("maintenance admission was not fenced: {reply:?}"),
        }
    }
}
pub(super) fn exercise(
    runtime: &tokio::runtime::Runtime,
    client: &Client,
    selection: &Selection,
    host: &wrix_state::host::Host,
) {
    let pool = Admission::acquire(selection, system())
        .unwrap()
        .pool()
        .clone();
    let root = host
        .roots()
        .state()
        .join("control/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str());
    let read = || {
        serde_json::from_slice::<serde_json::Value>(&fs::read(root.join("backend.json")).unwrap())
            .unwrap()
    };
    let original = read();
    let package = package();
    let invalid = host.roots().state().join("control/nix/not-a-repository");
    fs::write(&invalid, b"owned malformed fixture inventory control").unwrap();
    assert!(Repair::begin(host, &package).is_err());
    fs::remove_file(invalid).unwrap();
    assert_eq!(
        read(),
        original,
        "bad peer inventory mutated a serving pool"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let repair = loop {
        match Repair::begin(host, &package) {
            Ok(repair) => break repair,
            Err(Error::Backend(backend::Error::Busy)) => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("native repair drain failed: {error}"),
        }
    };
    assert_eq!(read()["state"]["phase"], "released");
    fenced(runtime, client, selection);
    drop(repair); // A lost host command must leave durable admission fencing, not automatically resume a VM.
    assert!(
        host.roots()
            .state()
            .join("nix-pool-maintenance.json")
            .exists()
    );
    fenced(runtime, client, selection);
    let mut conflict = serde_json::to_value(&package).unwrap();
    conflict["image"] =
        serde_json::json!("/nix/store/00000000000000000000000000000000-other-image.drv");
    let conflict: Package = serde_json::from_value(conflict).unwrap();
    assert!(matches!(
        Repair::begin(host, &conflict),
        Err(Error::Conflict)
    ));
    assert_eq!(read()["state"]["phase"], "released");
    let repair = Repair::begin(host, &package).unwrap();
    assert_eq!(
        runtime
            .block_on(client.request(Operation::InspectNixPool {}))
            .unwrap(),
        Reply::PoolInterface { version: 1 }
    );
    repair.complete().unwrap();
    assert!(
        !host
            .roots()
            .state()
            .join("nix-pool-maintenance.json")
            .exists()
    );
    let resumed = read();
    assert_eq!(resumed["state"]["phase"], "serving");
    assert_ne!(
        resumed["state"]["connection"]["target"]["id"],
        original["state"]["connection"]["target"]["id"]
    );
    assert_eq!(
        resumed["state"]["connection"]["target"]["attempt"]["software"],
        original["state"]["connection"]["target"]["attempt"]["software"]
    );
    let id = runtime
        .block_on(client.prepare_nix_pool(selection, system()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match runtime
            .block_on(client.request(Operation::InspectNixPoolPreparation { id: id.clone() }))
            .unwrap()
        {
            Reply::PoolPreparation {
                status: PoolPreparation::Ready,
            } => break,
            Reply::PoolPreparation {
                status: PoolPreparation::Pending,
            } => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(25));
            }
            reply => panic!("resumed native pool was unavailable: {reply:?}"),
        }
    }
}
