use super::*;
use std::os::unix::{fs::PermissionsExt, net::UnixListener};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<tempfile::TempDir> {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir().canonicalize()?)?;
    crate::dolt::private_directory(&root.path().join("run"))?;
    fs::write(root.path().join("owner.lock"), b"")?;
    fs::write(root.path().join("retained-database"), b"retained")?;
    Ok(root)
}

#[test]
fn dead_socket_is_retired_without_replacing_ownership_or_data() -> TestResult {
    let root = fixture()?;
    let owner = fs::metadata(root.path().join("owner.lock"))?;
    drop(UnixListener::bind(root.path().join("run/dolt.sock"))?);
    assert!(!ready(root.path()));
    retire(root.path())?;
    assert!(!root.path().join("run/dolt.sock").exists());
    assert_eq!(
        owner.ino(),
        fs::metadata(root.path().join("owner.lock"))?.ino()
    );
    assert_eq!(
        fs::read(root.path().join("retained-database"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn live_endpoint_is_not_unlinked() -> TestResult {
    let root = fixture()?;
    let _listener = UnixListener::bind(root.path().join("run/dolt.sock"))?;
    assert!(ready(root.path()));
    assert!(matches!(retire(root.path()), Err(Error::SocketBusy { .. })));
    assert!(root.path().join("run/dolt.sock").exists());
    Ok(())
}

#[test]
fn regular_files_and_symlinks_are_not_retired_as_sockets() -> TestResult {
    let root = fixture()?;
    let path = root.path().join("run/dolt.sock");
    fs::write(&path, b"evidence")?;
    assert!(matches!(retire(root.path()), Err(Error::UnsafePath)));
    assert_eq!(fs::read(&path)?, b"evidence");
    fs::remove_file(&path)?;
    std::os::unix::fs::symlink(root.path().join("retained-database"), &path)?;
    assert!(matches!(retire(root.path()), Err(Error::UnsafePath)));
    assert_eq!(
        fs::read(root.path().join("retained-database"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn original_owner_lock_excludes_endpoint_retirement() -> TestResult {
    let root = fixture()?;
    drop(UnixListener::bind(root.path().join("run/dolt.sock"))?);
    let owner = File::open(root.path().join("owner.lock"))?;
    owner.lock_exclusive()?;
    assert!(retire(root.path()).is_err());
    assert!(root.path().join("run/dolt.sock").exists());
    Ok(())
}
