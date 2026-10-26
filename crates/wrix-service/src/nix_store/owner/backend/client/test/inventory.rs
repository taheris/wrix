use super::*;

#[test]
fn protection_inventory_counts_uncertain_and_live_records_but_not_released_history() {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let control = directory.path().canonicalize().unwrap();
    let pool = Key {
        repository: "a".repeat(32).try_into().unwrap(),
        system: wrix_core::nix_store::pool::System::X86_64Linux,
    };
    let mut indexed = BTreeSet::new();
    let executions = control.join("executions");
    wrix_core::private_directory::prepare(&executions).unwrap();
    for (digit, state) in [
        ('a', State::Registering),
        ('b', State::Attaching),
        ('c', State::Detached),
        ('d', State::Released),
    ] {
        let ticket = Ticket::parse(&digit.to_string().repeat(64)).unwrap();
        let root = executions.join(ticket.as_str());
        wrix_core::private_directory::prepare(&root).unwrap();
        indexed.insert(ticket.clone());
        Record {
            version: 1,
            plane: Plane::Host,
            pool: pool.clone(),
            namespace: crate::control::Namespace::from(
                &wrix_core::path::WorkspaceHash::parse(&"b".repeat(64)).unwrap(),
            ),
            generation: vec![7; 32],
            ticket,
            lease: "c".repeat(32).try_into().unwrap(),
            state,
        }
        .publish(&root)
        .unwrap();
    }
    assert_eq!(retained(&control, &pool, &indexed).unwrap(), 3);
    indexed.remove(&Ticket::parse(&"d".repeat(64)).unwrap());
    assert_eq!(retained(&control, &pool, &indexed).unwrap(), 3);
    indexed.remove(&Ticket::parse(&"a".repeat(64)).unwrap());
    assert!(retained(&control, &pool, &indexed).is_err());
    assert_eq!(fs::read_dir(executions).unwrap().count(), 4);
}
