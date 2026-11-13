use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
fn fixture() -> (tempfile::TempDir, root::Root, PathBuf, serde_json::Value) {
    let directory = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(directory.path()).unwrap();
    let root = root::Root::parse(&path, root::Origin::Registered).unwrap();
    let private = path.join("private-user-manager");
    fs::create_dir(&private).unwrap();
    let receipt = private.join("ownership.json");
    let group = Path::new("/sys/fs/cgroup").join(format!(
        "wx-doaia-public-{}.scope",
        path.file_name().unwrap().to_str().unwrap()
    ));
    assert!(!group.exists());
    let proof = serde_json::json!({"version":1,"pid":std::process::id(),"cgroup":group,"device":0,"inode":0,"executable":"/owned/systemd","start_ticks":"0"});
    (directory, root, receipt, proof)
}
fn write(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn missing_private_manager_is_not_a_cleanup_capability() {
    let (_directory, root, _path, _proof) = fixture();
    assert!(Manager::read(&root).unwrap().is_none());
}
#[test]
fn manager_requires_exact_version_private_file_and_owned_cgroup_name() {
    let (_directory, root, path, proof) = fixture();
    for (key, value) in [
        ("version", serde_json::json!(2)),
        ("cgroup", serde_json::json!("/tmp/peer.scope")),
        ("start_ticks", serde_json::json!("bad")),
        ("extra", serde_json::json!(true)),
    ] {
        let mut changed = proof.clone();
        changed[key] = value;
        write(&path, &changed);
        assert!(Manager::read(&root).is_err());
    }
    write(&path, &proof);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Manager::read(&root).is_err());
}
#[test]
fn symlinked_or_hardlinked_manager_proof_never_authorizes_a_stop() {
    let (_directory, root, path, proof) = fixture();
    let peer = path.with_file_name("peer");
    write(&peer, &proof);
    symlink(&peer, &path).unwrap();
    assert!(Manager::read(&root).is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(&peer, &path).unwrap();
    assert!(Manager::read(&root).is_err());
}
#[test]
#[cfg(target_os = "linux")]
fn matching_pid_without_matching_birth_identity_cannot_control_the_manager() {
    let (_directory, root, path, proof) = fixture();
    write(&path, &proof);
    let manager = Manager::read(&root).unwrap().unwrap();
    assert!(matches!(
        manager.alive(),
        Err(Error::Policy("private manager birth identity changed"))
    ));
    assert!(
        manager
            .stop_controller(&root, &command::Environment::private(root.path()), None)
            .is_err()
    );
}
