use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let runtime = root.path().join("runtime");
    for path in [&cache, &runtime.join("backend"), &runtime.join("compiler")] {
        crate::dolt::private_directory(path).unwrap();
    }
    fs::write(runtime.join("backend/planned-stop"), b"old planned stop").unwrap();
    fs::write(runtime.join("compiler/released"), b"0\n").unwrap();
    (root, cache, runtime)
}

#[test]
fn next_native_start_drops_previous_runtime_release_without_replacing_the_cache_owner_inode() {
    let (_root, cache, runtime) = fixture();
    let owner = lock(&cache.join("owner.lock")).unwrap();
    let inode = owner.metadata().unwrap().ino();
    drop(owner);
    let _startup = prepare(&cache, &runtime).unwrap();
    assert!(!runtime.join("backend/planned-stop").exists());
    assert!(!runtime.join("compiler/released").exists());
    assert_eq!(fs::metadata(cache.join("owner.lock")).unwrap().ino(), inode);
    assert!(lock(&cache.join("owner.lock")).is_ok());
    assert!(prepare(&cache, &runtime).is_err());
}

#[test]
fn a_busy_original_compiler_preserves_all_previous_release_markers() {
    let (_root, cache, runtime) = fixture();
    let _owner = lock(&cache.join("owner.lock")).unwrap();
    assert!(prepare(&cache, &runtime).is_err());
    assert_eq!(
        fs::read(runtime.join("backend/planned-stop")).unwrap(),
        b"old planned stop"
    );
    assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
}

#[test]
fn substituted_marker_refuses_the_entire_cleanup_before_any_old_release_is_deleted() {
    let (root, cache, runtime) = fixture();
    let target = root.path().join("foreign");
    fs::write(&target, b"foreign payload").unwrap();
    fs::remove_file(runtime.join("compiler/released")).unwrap();
    symlink(&target, runtime.join("compiler/released")).unwrap();
    assert!(prepare(&cache, &runtime).is_err());
    assert_eq!(
        fs::read(runtime.join("backend/planned-stop")).unwrap(),
        b"old planned stop"
    );
    assert_eq!(fs::read(target).unwrap(), b"foreign payload");
}

#[test]
fn malformed_previous_pid_is_not_normalized_into_cleanup_authority() {
    let (_root, cache, runtime) = fixture();
    for value in ["+1\n", "01\n", "0\n", "-1\n", "1\n\n", "2147483648\n"] {
        fs::write(runtime.join("compiler/pid"), value).unwrap();
        assert!(prepare(&cache, &runtime).is_err());
        assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
        assert_eq!(
            fs::read_to_string(runtime.join("compiler/pid")).unwrap(),
            value
        );
    }
}

#[test]
fn a_live_or_uncertain_previous_pid_is_not_retired_as_stale() {
    let (_root, cache, runtime) = fixture();
    let pid = runtime.join("compiler/pid");
    fs::write(&pid, std::process::id().to_string()).unwrap();
    fs::set_permissions(&pid, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(prepare(&cache, &runtime).is_err());
    assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
    assert!(pid.exists());
}
