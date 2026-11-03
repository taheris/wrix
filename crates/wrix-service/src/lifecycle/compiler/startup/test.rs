use super::*;
use std::os::unix::{
    fs::{PermissionsExt, symlink},
    net::UnixListener,
};

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
fn dead_owned_native_endpoints_are_retired_without_replacing_cache_ownership() {
    let (_root, cache, runtime) = fixture();
    let owner = lock(&cache.join("owner.lock")).unwrap();
    let inode = owner.metadata().unwrap().ino();
    drop(owner);
    for name in ["sccache.sock", "drain.sock", "native.sock"] {
        drop(UnixListener::bind(runtime.join("compiler").join(name)).unwrap());
    }
    let _startup = prepare(&cache, &runtime).unwrap();
    for name in ["sccache.sock", "drain.sock", "native.sock"] {
        let path = runtime.join("compiler").join(name);
        assert!(!path.exists());
        // Exercise the native bind that failed during restart, not just absence.
        let _replacement = UnixListener::bind(path).unwrap();
    }
    assert_eq!(fs::metadata(cache.join("owner.lock")).unwrap().ino(), inode);
}

#[test]
fn a_live_native_endpoint_preserves_dead_endpoints_and_release_markers() {
    for live in ["sccache.sock", "drain.sock", "native.sock"] {
        let (_root, cache, runtime) = fixture();
        let mut listeners = Vec::new();
        for name in ["sccache.sock", "drain.sock", "native.sock"] {
            let listener = UnixListener::bind(runtime.join("compiler").join(name)).unwrap();
            if name == live {
                listeners.push(listener);
            }
        }
        assert_eq!(listeners.len(), 1);
        assert!(prepare(&cache, &runtime).is_err());
        for name in ["sccache.sock", "drain.sock", "native.sock"] {
            assert!(runtime.join("compiler").join(name).exists());
        }
        assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
        assert_eq!(
            fs::read(runtime.join("backend/planned-stop")).unwrap(),
            b"old planned stop"
        );
    }
}

#[test]
fn compiler_ownership_excludes_dead_endpoint_retirement() {
    let (_root, cache, runtime) = fixture();
    let path = runtime.join("compiler/sccache.sock");
    drop(UnixListener::bind(&path).unwrap());
    let _owner = lock(&cache.join("owner.lock")).unwrap();
    assert!(prepare(&cache, &runtime).is_err());
    assert!(path.exists());
    assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
}

#[test]
fn malformed_native_endpoint_preserves_the_entire_startup_state() {
    for name in ["sccache.sock", "drain.sock", "native.sock"] {
        let (root, cache, runtime) = fixture();
        let path = runtime.join("compiler").join(name);
        fs::write(&path, b"not a socket").unwrap();
        assert!(prepare(&cache, &runtime).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not a socket");
        fs::remove_file(&path).unwrap();
        let target = root.path().join("foreign.sock");
        drop(UnixListener::bind(&target).unwrap());
        symlink(&target, &path).unwrap();
        assert!(prepare(&cache, &runtime).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(target.exists());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(prepare(&cache, &runtime).is_err());
        assert_eq!(fs::metadata(&target).unwrap().nlink(), 2);
        assert_eq!(fs::read(runtime.join("compiler/released")).unwrap(), b"0\n");
        assert_eq!(
            fs::read(runtime.join("backend/planned-stop")).unwrap(),
            b"old planned stop"
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
