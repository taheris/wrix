mod inventory;
use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn interrupted_client_ownership_is_protected_and_substituted_records_are_never_empty_successes() {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let control = directory.path().canonicalize().unwrap();
    let ticket = Ticket::parse(&"a".repeat(64)).unwrap();
    let indexed = BTreeSet::from([ticket.clone()]);
    let pool = Key {
        repository: "a".repeat(32).try_into().unwrap(),
        system: wrix_core::nix_store::pool::System::X86_64Linux,
    };
    let root = control.join("executions").join(ticket.as_str());
    wrix_core::private_directory::prepare(&control.join("executions")).unwrap();
    wrix_core::private_directory::prepare(&root).unwrap();
    let mut record = Record {
        version: 1,
        plane: Plane::Host,
        pool: pool.clone(),
        namespace: crate::control::Namespace::from(
            &wrix_core::path::WorkspaceHash::parse(&"b".repeat(64)).unwrap(),
        ),
        generation: vec![7; 32],
        ticket,
        lease: "c".repeat(32).try_into().unwrap(),
        state: State::Registering,
        native_process: None,
        broker_epoch: None,
        port: None,
    };
    let check = || protected(&control, &pool, &indexed);
    record.publish(&root).unwrap();
    assert!(check().unwrap());
    assert_eq!(retained(&control, &pool, &indexed).unwrap(), 1);
    record.state = State::Attaching;
    record.publish(&root).unwrap();
    assert!(check().unwrap());
    record.state = State::Detached;
    record.publish(&root).unwrap();
    assert!(check().unwrap());
    record.state = State::Released;
    record.publish(&root).unwrap();
    assert!(!check().unwrap());
    assert_eq!(retained(&control, &pool, &indexed).unwrap(), 0);
    fs::hard_link(root.join("client.json"), root.join("substituted")).unwrap();
    assert!(check().is_err());
    fs::rename(control.join("executions"), control.join("retained")).unwrap();
    assert!(check().is_err());
}
