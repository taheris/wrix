use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

fn key(byte: u8) -> Key {
    let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    wire.extend([byte; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(wire))
        .try_into()
        .unwrap()
}

fn software(byte: char) -> StorePath {
    format!("/nix/store/{}-pool-bootstrap", byte.to_string().repeat(32))
        .try_into()
        .unwrap()
}

fn private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    for name in ["owner-active", "leases", "incoming", "filesystem"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join(name))
            .unwrap();
    }
    fs::DirBuilder::new()
        .mode(0o755)
        .create(root.path().join("export-root"))
        .unwrap();
    // DirBuilder's requested public mode is masked by a restrictive caller umask.
    fs::set_permissions(
        root.path().join("export-root"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    private(
        &root.path().join("owner-active/version"),
        b"2:test:8589934592\n",
    );
    seal(root.path(), &software('0'), &key(7)).unwrap();
    private(&root.path().join("released"), b"1\n");
    root
}

#[test]
fn explicit_upgrade_reseals_only_after_consuming_the_exact_previous_release() {
    let root = fixture();
    begin(root.path(), &software('0'), &key(7)).unwrap();
    upgrade(root.path(), &software('0'), &software('1'), &key(7)).unwrap();
    assert_eq!(
        read(&root.path().join("owner-active/software")).unwrap(),
        software('1').as_str().as_bytes()
    );
    assert_eq!(
        read(&root.path().join("owner-active/admin-key")).unwrap(),
        key(7).as_str().as_bytes()
    );
    assert_eq!(read(&root.path().join("resuming")).unwrap(), b"1\n");
    assert!(!root.path().join("released").exists());
}

#[test]
fn upgrade_cannot_skip_checkpoint_consumption() {
    let root = fixture();
    assert!(upgrade(root.path(), &software('0'), &software('1'), &key(7)).is_err());
    assert_eq!(
        read(&root.path().join("owner-active/software")).unwrap(),
        software('0').as_str().as_bytes()
    );
    assert_eq!(read(&root.path().join("released")).unwrap(), b"1\n");
}

#[test]
fn upgrade_rejects_changed_previous_software_or_administration() {
    for (previous, admin) in [(software('2'), key(7)), (software('0'), key(8))] {
        let root = fixture();
        begin(root.path(), &software('0'), &key(7)).unwrap();
        assert!(upgrade(root.path(), &previous, &software('1'), &admin).is_err());
        assert_eq!(
            read(&root.path().join("owner-active/software")).unwrap(),
            software('0').as_str().as_bytes()
        );
    }
}

#[test]
fn interrupted_upgrade_cannot_reuse_the_consumed_release() {
    let root = fixture();
    begin(root.path(), &software('0'), &key(7)).unwrap();
    upgrade(root.path(), &software('0'), &software('1'), &key(7)).unwrap();
    assert!(begin(root.path(), &software('1'), &key(7)).is_err());
    assert!(upgrade(root.path(), &software('0'), &software('2'), &key(7)).is_err());
    assert_eq!(
        read(&root.path().join("owner-active/software")).unwrap(),
        software('1').as_str().as_bytes()
    );
}

#[test]
fn resume_consumes_the_checkpoint_before_native_database_access() {
    let root = fixture();
    begin(root.path(), &software('0'), &key(7)).unwrap();
    assert!(!root.path().join("released").exists());
    assert_eq!(read(&root.path().join("resuming")).unwrap(), b"1\n");
    assert_eq!(
        read(&root.path().join("owner-active/software")).unwrap(),
        software('0').as_str().as_bytes()
    );
}

#[test]
fn missing_or_invalid_release_never_authorizes_resume() {
    for bytes in [None, Some(b"0\n".as_slice()), Some(b"1\nextra".as_slice())] {
        let root = fixture();
        fs::remove_file(root.path().join("released")).unwrap();
        if let Some(bytes) = bytes {
            private(&root.path().join("released"), bytes);
        }
        assert!(begin(root.path(), &software('0'), &key(7)).is_err());
        assert!(!root.path().join("resuming").exists());
    }
}

#[test]
fn retained_lease_or_unknown_reader_record_prevents_resume() {
    let root = fixture();
    private(
        &root.path().join("leases/unknown-reader"),
        b"incomplete admission",
    );
    assert!(matches!(
        begin(root.path(), &software('0'), &key(7)),
        Err(Error::Busy)
    ));
    assert_eq!(read(&root.path().join("released")).unwrap(), b"1\n");
    assert!(!root.path().join("resuming").exists());
}

#[test]
fn changed_software_and_administration_cannot_reuse_old_release() {
    for (software, key) in [(software('1'), key(7)), (software('0'), key(8))] {
        let root = fixture();
        assert!(matches!(
            begin(root.path(), &software, &key),
            Err(Error::Invalid)
        ));
        assert_eq!(read(&root.path().join("released")).unwrap(), b"1\n");
        assert!(!root.path().join("resuming").exists());
    }
}

#[test]
fn interrupted_resume_cannot_replay_a_copied_checkpoint() {
    let root = fixture();
    begin(root.path(), &software('0'), &key(7)).unwrap();
    private(&root.path().join("released"), b"1\n");
    assert!(begin(root.path(), &software('0'), &key(7)).is_err());
    assert_eq!(read(&root.path().join("resuming")).unwrap(), b"1\n");
}

#[test]
fn symlinked_or_shared_release_does_not_modify_its_target() {
    for link in [false, true] {
        let root = fixture();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("released");
        private(&target, b"1\n");
        fs::remove_file(root.path().join("released")).unwrap();
        if link {
            symlink(&target, root.path().join("released")).unwrap();
        } else {
            fs::hard_link(&target, root.path().join("released")).unwrap();
        }
        assert!(begin(root.path(), &software('0'), &key(7)).is_err());
        assert_eq!(fs::read(target).unwrap(), b"1\n");
        assert!(!root.path().join("resuming").exists());
    }
}

#[test]
fn sealing_never_overwrites_a_previous_native_identity() {
    let root = fixture();
    assert!(seal(root.path(), &software('1'), &key(8)).is_err());
    assert_eq!(
        read(&root.path().join("owner-active/software")).unwrap(),
        software('0').as_str().as_bytes()
    );
    assert_eq!(
        read(&root.path().join("owner-active/admin-key")).unwrap(),
        key(7).as_str().as_bytes()
    );
}
