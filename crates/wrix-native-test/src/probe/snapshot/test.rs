use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
fn root() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, path)
}
fn private(path: &Path, data: &[u8]) {
    fs::write(path, data).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn private_snapshot_reads_the_original_pinned_directory_after_rename() {
    let (_temp, path) = root();
    let old = path.join("before");
    fs::create_dir(&old).unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o700)).unwrap();
    private(&old.join("data"), b"original");
    let directory = Directory::open(&old).unwrap();
    let renamed = path.join("renamed");
    fs::rename(&old, &renamed).unwrap();
    fs::create_dir(&old).unwrap();
    private(&old.join("data"), b"substitute");
    assert_eq!(directory.read("data").unwrap(), b"original");
}
#[test]
fn symlink_and_hardlink_files_are_refused() {
    let (_temp, path) = root();
    private(&path.join("data"), b"value");
    let directory = Directory::open(&path).unwrap();
    symlink("data", path.join("alias")).unwrap();
    assert!(directory.read("alias").is_err());
    fs::hard_link(path.join("data"), path.join("linked")).unwrap();
    assert!(directory.read("data").is_err());
}
#[test]
fn traversal_and_group_readable_snapshots_are_refused() {
    let (_temp, path) = root();
    private(&path.join("data"), b"value");
    let directory = Directory::open(&path).unwrap();
    for name in ["", ".", "..", "../data", "/etc/passwd", "nested/data"] {
        assert!(directory.read(name).is_err());
    }
    fs::set_permissions(path.join("data"), fs::Permissions::from_mode(0o640)).unwrap();
    assert!(directory.read("data").is_err());
}
#[test]
fn fifo_is_refused_without_waiting_for_a_writer() {
    let (_temp, path) = root();
    assert!(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(path.join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    assert!(Directory::open(&path).unwrap().read("pipe").is_err());
}
#[test]
fn symlink_and_public_parent_directories_are_refused() {
    let (_temp, path) = root();
    symlink(&path, path.join("alias")).unwrap();
    assert!(Directory::open(&path.join("alias")).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Directory::open(&path).is_err());
}
