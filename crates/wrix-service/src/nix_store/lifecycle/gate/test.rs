use super::*;

#[test]
fn pending_update_refuses_admission_without_claiming_host_shutdown() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("update.json"), b"retained intent").unwrap();
    let error = check_services(root.path()).unwrap_err();
    assert!(matches!(error, Error::UpdatePending));
    assert!(error.to_string().contains("service update --cancel"));
    assert!(!error.to_string().contains("explicitly stopped"));
    assert_eq!(
        fs::read(root.path().join("update.json")).unwrap(),
        b"retained intent"
    );
}

#[test]
fn host_shutdown_remains_an_independent_admission_fence() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("shutdown.json"), b"shutdown").unwrap();
    assert!(matches!(check_services(root.path()), Err(Error::Stopped)));
}

#[test]
fn cancelled_update_evidence_does_not_fence_admission() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("cancelled-update-attempt.json"),
        b"evidence",
    )
    .unwrap();
    check_services(root.path()).unwrap();
}

#[test]
fn unsafe_pending_marker_is_not_ignored() {
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/missing", root.path().join("update.json")).unwrap();
    assert!(matches!(
        check_services(root.path()),
        Err(Error::UpdatePending)
    ));
}
