use super::discover_sockets;
use std::{collections::HashMap, fs, os::unix::fs::symlink};

#[test]
fn closed_descriptor_does_not_discard_live_server_sockets() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    symlink("socket:[123]", &live).unwrap();
    let descriptors = [
        Ok(root.path().join("closed-before")),
        Ok(live),
        Ok(root.path().join("closed-after")),
    ];
    assert_eq!(
        discover_sockets(descriptors, &HashMap::from([("123", "/live socket")])).unwrap(),
        ["/live socket"]
    );
}

#[test]
fn descriptor_errors_other_than_disappearance_are_reported() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("not-a-link");
    fs::write(&invalid, "").unwrap();
    assert!(discover_sockets([Ok(invalid)], &HashMap::new()).is_err());
}
