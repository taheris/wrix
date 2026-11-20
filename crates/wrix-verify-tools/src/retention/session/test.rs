use super::*;
use std::os::unix::fs::symlink;

fn temporary() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

const OUTPUT: &str = "/nix/store/00000000000000000000000000000000-ci-output";

#[test]
fn recipe_links_never_become_output_realization_requests() {
    assert!(root("derivation", Path::new(&format!("{OUTPUT}.drv"))).is_ok());
    assert!(root("runner", Path::new(&format!("{OUTPUT}.drv"))).is_err());
    assert!(root("derivation", Path::new(OUTPUT)).is_err());
    assert!(root("runner", Path::new("/tmp/foreign")).is_err());
    assert!(root("foreign", Path::new(OUTPUT)).is_err());
}

#[test]
fn leased_batches_survive_cleanup_until_the_descriptor_is_released() {
    let temporary = temporary();
    let namespace = Namespace::open(&temporary.path().join("cache")).unwrap();
    let batch = namespace.create(std::ffi::OsStr::new("sh")).unwrap();
    let directory = batch.directory.clone();
    symlink(OUTPUT, directory.join("runner")).unwrap();
    namespace.reap().unwrap();
    assert!(directory.join("runner").is_symlink());
    drop(batch);
    namespace.reap().unwrap();
    assert!(!directory.exists());
}

#[test]
fn unknown_batch_contents_refuse_retirement_without_deleting_known_roots() {
    let temporary = temporary();
    let namespace = Namespace::open(&temporary.path().join("cache")).unwrap();
    let batch = namespace.create(std::ffi::OsStr::new("sh")).unwrap();
    let directory = batch.directory.clone();
    symlink(OUTPUT, directory.join("runner")).unwrap();
    fs::write(directory.join("foreign"), "preserved").unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    assert!(directory.join("runner").is_symlink());
    assert_eq!(fs::read(directory.join("foreign")).unwrap(), b"preserved");
}

#[test]
fn interrupted_pending_manifest_without_registered_roots_is_safe_to_retire() {
    let temporary = temporary();
    let namespace = Namespace::open(&temporary.path().join("cache")).unwrap();
    let batch = namespace.create(std::ffi::OsStr::new("sh")).unwrap();
    let directory = batch.directory.clone();
    let pending = directory.join("pending");
    crate::retention::initialize(&pending).unwrap();
    let generation = pending.join("generation-aborted");
    fs::create_dir(&generation).unwrap();
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(generation.join("manifest"), "truncated-write").unwrap();
    symlink(format!("{OUTPUT}.drv"), directory.join("derivation")).unwrap();
    drop(batch);
    namespace.reap().unwrap();
    assert!(!directory.exists());
}

#[test]
fn modified_pending_root_refuses_retirement_of_the_entire_batch() {
    let temporary = temporary();
    let namespace = Namespace::open(&temporary.path().join("cache")).unwrap();
    let batch = namespace.create(std::ffi::OsStr::new("sh")).unwrap();
    let directory = batch.directory.clone();
    let pending = directory.join("pending");
    crate::retention::initialize(&pending).unwrap();
    let generation = pending.join("generation-modified");
    fs::create_dir(&generation).unwrap();
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(generation.join("manifest"), format!("{OUTPUT}\n")).unwrap();
    symlink(
        "/nix/store/00000000000000000000000000000000-foreign",
        generation.join("batch-0"),
    )
    .unwrap();
    symlink(format!("{OUTPUT}.drv"), directory.join("derivation")).unwrap();
    drop(batch);
    assert!(namespace.reap().is_err());
    assert!(directory.join("derivation").is_symlink());
    assert!(generation.join("batch-0").is_symlink());
}

#[test]
fn foreign_directories_and_lock_symlinks_are_never_adopted() {
    let temporary = temporary();
    let path = temporary.path().join("cache");
    fs::create_dir(&path).unwrap();
    assert!(Namespace::open(&path).is_err());
    fs::remove_dir(&path).unwrap();
    let namespace = Namespace::open(&path).unwrap();
    drop(namespace);
    fs::remove_file(path.join("lock")).unwrap();
    let foreign = temporary.path().join("foreign");
    fs::write(&foreign, "preserved").unwrap();
    symlink(&foreign, path.join("lock")).unwrap();
    assert!(Namespace::open(&path).is_err());
    assert_eq!(fs::read(foreign).unwrap(), b"preserved");
}

#[test]
fn inherited_batch_pointer_needs_the_exact_descriptor_and_program() {
    let temporary = temporary();
    let namespace = Namespace::open(&temporary.path().join("cache")).unwrap();
    let batch = namespace.create(std::ffi::OsStr::new("sh")).unwrap();
    let descriptor = batch.lease.as_raw_fd().to_string();
    assert!(check(&batch.directory, &descriptor, std::ffi::OsStr::new("sh")).is_ok());
    assert!(
        check(
            &batch.directory,
            &descriptor,
            std::ffi::OsStr::new("foreign")
        )
        .is_err()
    );
    assert!(
        check(
            &batch.directory,
            "not-a-descriptor",
            std::ffi::OsStr::new("sh")
        )
        .is_err()
    );
    let other = tempfile::tempfile().unwrap();
    assert!(
        check(
            &batch.directory,
            &other.as_raw_fd().to_string(),
            std::ffi::OsStr::new("sh")
        )
        .is_err()
    );
}
