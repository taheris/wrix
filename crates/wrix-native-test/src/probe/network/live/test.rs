use super::*;
#[test]
fn ordinary_filesystem_never_becomes_a_mutable_network_fixture() {
    let root = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(root.path()).unwrap();
    assert!(matches!(admit(&path), Err(Error::Boundary)));
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}
#[test]
fn ambient_or_mutable_programs_are_refused_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("nft");
    fs::write(&path, b"never execute").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(executable(&path), Err(Error::Boundary)));
    assert_eq!(fs::read(path).unwrap(), b"never execute");
}
#[test]
fn pinned_resource_identity_detects_replacement_without_deleting_foreign_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned");
    fs::write(&path, b"original").unwrap();
    let identity = Identity::pin(&path).unwrap();
    fs::rename(&path, root.path().join("retained-original")).unwrap();
    fs::write(&path, b"replacement").unwrap();
    assert!(matches!(identity.check(&path), Err(Error::Ownership)));
    assert_eq!(fs::read(path).unwrap(), b"replacement");
}
#[test]
fn reverse_cleanup_preserves_a_replaced_directory_and_reports_uncertainty() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("owned");
    fs::create_dir(&path).unwrap();
    let identity = Identity::pin(&path).unwrap();
    fs::rename(&path, files.path().join("retained-original")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("foreign"), b"keep").unwrap();
    let mut run = Run {
        tools: Tools {
            ip: PathBuf::from("/never/ip"),
            nft: PathBuf::from("/never/nft"),
            setpriv: PathBuf::from("/never/setpriv"),
            ca: PathBuf::from("/never/ca"),
        },
        proc: PathBuf::from("/never/proc"),
        namespace: PathBuf::from("/never/namespace"),
        net_identity: fs::read_link("/proc/self/ns/net").unwrap(),
        files,
        undo: vec![Undo::Directory(path.clone(), identity)],
    };
    assert_eq!(run.cleanup().len(), 1);
    assert_eq!(fs::read(path.join("foreign")).unwrap(), b"keep");
    assert!(run.undo.is_empty());
}
#[test]
fn reverse_cleanup_preserves_a_foreign_forwarding_value_and_reports_uncertainty() {
    let files = tempfile::tempdir().unwrap();
    let proc = files.path().join("proc");
    let directory = proc.join("sys/net/ipv4");
    fs::create_dir_all(&directory).unwrap();
    let forward = directory.join("ip_forward");
    fs::write(&forward, b"foreign\n").unwrap();
    let mut run = Run {
        tools: Tools {
            ip: PathBuf::from("/never/ip"),
            nft: PathBuf::from("/never/nft"),
            setpriv: PathBuf::from("/never/setpriv"),
            ca: PathBuf::from("/never/ca"),
        },
        proc,
        namespace: PathBuf::from("/never/namespace"),
        net_identity: fs::read_link("/proc/self/ns/net").unwrap(),
        files,
        undo: vec![Undo::Forward(b"0\n".to_vec())],
    };
    assert_eq!(run.cleanup().len(), 1);
    assert_eq!(fs::read(&forward).unwrap(), b"foreign\n");
}
#[test]
fn mutable_certificate_input_is_refused_before_connecting() {
    let files = tempfile::tempdir().unwrap();
    let certificate = files.path().join("ca.pem");
    fs::write(&certificate, b"not a certificate").unwrap();
    assert!(matches!(store_file(&certificate), Err(Error::Boundary)));
    assert_eq!(fs::read(certificate).unwrap(), b"not a certificate");
}
