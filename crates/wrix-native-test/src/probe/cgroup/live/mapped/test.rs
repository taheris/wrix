use super::*;
use std::os::unix::fs::PermissionsExt;
#[test]
fn arbitrary_filesystem_tool_directories_are_refused_before_namespace_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(root.path()).unwrap();
    assert!(Package::parse(&path).is_err());
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}
#[test]
fn writable_parent_budget_is_a_failed_probe_not_successful_delegation() {
    let root = tempfile::tempdir().unwrap();
    for name in ["memory.max", "pids.max", "cpu.max"] {
        fs::write(root.path().join(name), b"123\n").unwrap();
    }
    assert!(matches!(
        refuse_budget_writes(root.path()),
        Err(Error::Evidence)
    ));
}
#[test]
fn permission_denial_is_distinguished_from_missing_parent_controls() {
    let root = tempfile::tempdir().unwrap();
    for name in ["memory.max", "pids.max", "cpu.max"] {
        let path = root.path().join(name);
        fs::write(&path, b"123\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    }
    if rustix::process::geteuid().is_root() {
        assert!(matches!(
            refuse_budget_writes(root.path()),
            Err(Error::Evidence)
        ));
    } else {
        refuse_budget_writes(root.path()).unwrap();
    }
    fs::remove_file(root.path().join("memory.max")).unwrap();
    assert!(refuse_budget_writes(root.path()).is_err());
}
#[test]
fn unknown_or_incomplete_worker_modes_fail_without_kernel_operations() {
    assert!(Worker::parse("unknown".as_ref()).is_err());
    for operation in [Worker::Bootstrap, Worker::Delegate, Worker::Job] {
        assert!(matches!(worker(operation, &[]), Err(Error::Usage)));
    }
}
