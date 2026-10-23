use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::os::unix::fs::{PermissionsExt, symlink};
use wrix_core::nix_store::pool::{Repository, System};

fn layout(root: &Path) -> deployment::Layout {
    let root = root.canonicalize().unwrap();
    deployment::Layout::new(&root, &root.join("cache")).unwrap()
}

fn pool() -> Key {
    Key {
        repository: Repository::try_from("a".repeat(32)).unwrap(),
        system: System::X86_64Linux,
    }
}

fn deployment() -> deployment::Id {
    "b".repeat(32).try_into().unwrap()
}

fn key() -> lease::Key {
    let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    wire.extend([7_u8; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(wire))
        .try_into()
        .unwrap()
}

#[test]
fn caller_loss_retains_reservation_and_never_implies_native_release() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let reservation = Reservation::create(&layout, &deployment(), &pool()).unwrap();
    let paths = Paths::new(&layout, &pool());
    let record = Record::read(&paths.control).unwrap();
    assert_eq!(record.phase, Phase::Reserved);
    assert_eq!(fs::read_dir(&paths.data).unwrap().count(), 0);
    let lock = private_file(&paths.control, "ownership", false).unwrap();
    assert_eq!(
        FileExt::try_lock_exclusive(&lock).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(reservation);
    FileExt::try_lock_exclusive(&lock).unwrap();
    assert!(matches!(
        Reservation::create(&layout, &deployment(), &pool()),
        Err(Error::Retained)
    ));
    assert_eq!(Record::read(&paths.control).unwrap(), record);
}

#[test]
fn preparation_is_disjoint_per_repository_and_guest_system() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let first = Reservation::create(&layout, &deployment(), &pool()).unwrap();
    for other in [
        Key {
            system: System::Aarch64Linux,
            ..pool()
        },
        Key {
            repository: Repository::try_from("c".repeat(32)).unwrap(),
            ..pool()
        },
    ] {
        let next = Reservation::create(&layout, &deployment(), &other).unwrap();
        assert_ne!(first.paths.control, next.paths.control);
        assert_ne!(first.paths.data, next.paths.data);
        assert!(!next.paths.data.starts_with(layout.control_root()));
        assert_eq!(Record::read(&next.paths.control).unwrap().pool, other);
    }
}

#[test]
fn unexplained_service_data_is_not_adopted_or_changed() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let paths = Paths::new(&layout, &pool());
    create_directory(&paths.data).unwrap();
    fs::write(paths.data.join("retained"), b"history").unwrap();
    assert!(matches!(
        Reservation::create(&layout, &deployment(), &pool()),
        Err(Error::Retained)
    ));
    assert_eq!(fs::read(paths.data.join("retained")).unwrap(), b"history");
}

#[test]
fn preexisting_disk_is_not_resized_truncated_or_formatted_on_allocation() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let reservation = Reservation::create(&layout, &deployment(), &pool()).unwrap();
    let paths = Paths::new(&layout, &pool());
    fs::write(paths.data.join("store.ext4"), b"retained disk").unwrap();
    assert!(matches!(reservation.allocate(key()), Err(Error::Retained)));
    assert_eq!(
        fs::read(paths.data.join("store.ext4")).unwrap(),
        b"retained disk"
    );
    assert_eq!(Record::read(&paths.control).unwrap().phase, Phase::Reserved);
}

#[test]
fn substituted_control_root_does_not_expose_or_rewrite_its_target() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let paths = Paths::new(&layout, &pool());
    wrix_core::private_directory::prepare_with_parents(paths.control.parent().unwrap()).unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("keep"), b"authority").unwrap();
    symlink(target.path(), &paths.control).unwrap();
    assert!(Reservation::create(&layout, &deployment(), &pool()).is_err());
    assert_eq!(fs::read(target.path().join("keep")).unwrap(), b"authority");
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 1);
}

#[test]
fn changed_or_nonprivate_metadata_fails_before_disk_allocation() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let reservation = Reservation::create(&layout, &deployment(), &pool()).unwrap();
    let paths = Paths::new(&layout, &pool());
    fs::set_permissions(
        paths.control.join(RECORD),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(reservation.allocate(key()).is_err());
    assert_eq!(fs::read_dir(paths.data).unwrap().count(), 0);
}

#[test]
#[ignore = "requires nine GiB of free native storage; execute explicitly for pool admission acceptance"]
fn native_allocation_reserves_full_capacity_and_retains_single_owner_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let layout = layout(directory.path());
    let reservation = Reservation::create(&layout, &deployment(), &pool()).unwrap();
    let prepared = reservation.allocate(key()).unwrap();
    assert_eq!(prepared.pool(), &pool());
    let disk = File::open(prepared.data_root().join("store.ext4")).unwrap();
    assert_eq!(disk.metadata().unwrap().len(), bootstrap::CAPACITY);
    assert!(FileExt::allocated_size(&disk).unwrap() >= bootstrap::CAPACITY);
    let configuration: bootstrap::Configuration =
        serde_json::from_slice(&fs::read(prepared.data_root().join("bootstrap.json")).unwrap())
            .unwrap();
    assert_eq!(configuration.storage(), bootstrap::Storage::Preallocated);
    let paths = Paths::new(&layout, &pool());
    assert_eq!(Record::read(&paths.control).unwrap().phase, Phase::Prepared);
    drop(prepared);
    assert!(matches!(
        Reservation::create(&layout, &deployment(), &pool()),
        Err(Error::Retained)
    ));
    assert!(paths.data.join("store.ext4").exists());
}
