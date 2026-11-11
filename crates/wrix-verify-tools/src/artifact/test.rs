use super::*;

fn entry(kind: Kind, mode: u32) -> Entry {
    Entry { kind, mode }
}

fn layer(entries: &[(&str, Kind)]) -> Layer {
    Layer::parse(
        entries
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), entry(kind.clone(), 0o444))),
    )
    .unwrap()
}

fn hardlink(target: &str) -> Kind {
    Kind::Hardlink(Name::parse(target).unwrap())
}

#[test]
fn normalization_retains_posix_names_and_rejects_parent_components() {
    for (raw, normalized) in [
        ("/./etc//marker", "etc/marker"),
        ("///", "."),
        ("", "."),
        ("etc/..hidden", "etc/..hidden"),
        ("etc/back\\slash", "etc/back\\slash"),
        ("café/entry", "café/entry"),
    ] {
        assert_eq!(Name::parse(raw).unwrap().as_str(), normalized);
    }
    for raw in ["../outside", "/etc/../outside", "a/..", "./../"] {
        assert!(matches!(Name::parse(raw), Err(Error::UnsafeName(_))));
    }
}

#[test]
fn normalized_inner_headers_retain_the_last_entry() {
    let layer = Layer::parse([
        ("./etc/marker".to_owned(), entry(Kind::Regular, 0o444)),
        ("/etc/marker".to_owned(), entry(Kind::Regular, 0o755)),
    ])
    .unwrap();
    let selected = Selection::new([&layer]);
    let name = Name::parse("etc/marker").unwrap();
    assert_eq!(selected.lookup(&name).unwrap().entry.mode, 0o755);
    assert_eq!(selected.names().len(), 1);
}

#[test]
fn last_selected_layer_wins_without_including_unselected_tiers() {
    let bottom = layer(&[("marker", Kind::Regular), ("removed", Kind::Regular)]);
    let top = layer(&[("marker", Kind::Symlink("target".to_owned()))]);
    let name = Name::parse("marker").unwrap();
    let selected = Selection::new([&bottom, &top]);
    assert_eq!(selected.lookup(&name).unwrap().layer, 1);
    assert_eq!(
        selected.lookup(&name).unwrap().entry.kind,
        Kind::Symlink("target".to_owned())
    );
    assert_eq!(
        Selection::new([&bottom]).lookup(&name).unwrap().entry.kind,
        Kind::Regular
    );
    assert!(Selection::new([]).lookup(&name).is_none());
}

#[test]
fn whiteouts_hide_files_and_directory_descendants_but_not_neighbor_names() {
    let bottom = layer(&[
        ("etc/removed", Kind::Regular),
        ("nix/store/file", Kind::Regular),
        ("nix/storehouse/file", Kind::Regular),
    ]);
    let top = layer(&[
        ("etc/.wh.removed", Kind::Regular),
        ("nix/.wh.store", Kind::Regular),
    ]);
    let selected = Selection::new([&bottom, &top]);
    for hidden in ["etc/removed", "nix/store/file"] {
        assert!(selected.lookup(&Name::parse(hidden).unwrap()).is_none());
        assert!(
            Selection::new([&bottom])
                .lookup(&Name::parse(hidden).unwrap())
                .is_some()
        );
    }
    assert!(
        selected
            .lookup(&Name::parse("nix/storehouse/file").unwrap())
            .is_some()
    );
}

#[test]
fn same_layer_recreation_wins_over_whiteout() {
    let top = layer(&[
        ("etc/.wh.removed", Kind::Regular),
        ("etc/removed", Kind::Regular),
    ]);
    assert!(
        Selection::new([&top])
            .lookup(&Name::parse("etc/removed").unwrap())
            .is_some()
    );
}

#[test]
fn opaque_directory_hides_only_lower_children() {
    let bottom = layer(&[
        ("opt/hidden/old", Kind::Regular),
        ("opt/visible", Kind::Regular),
    ]);
    let top = layer(&[
        ("opt/hidden/.wh..wh..opq", Kind::Regular),
        ("opt/hidden/new", Kind::Regular),
    ]);
    let selected = Selection::new([&bottom, &top]);
    assert!(
        selected
            .lookup(&Name::parse("opt/hidden/old").unwrap())
            .is_none()
    );
    for visible in ["opt/hidden/new", "opt/visible"] {
        assert!(selected.lookup(&Name::parse(visible).unwrap()).is_some());
    }
}

#[test]
fn root_opaque_marker_hides_lower_root_children() {
    let bottom = layer(&[("old", Kind::Regular)]);
    let top = layer(&[(".wh..wh..opq", Kind::Regular)]);
    assert!(
        Selection::new([&bottom, &top])
            .lookup(&Name::parse("old").unwrap())
            .is_none()
    );
    assert!(
        Selection::new([&bottom])
            .lookup(&Name::parse("old").unwrap())
            .is_some()
    );
}

#[test]
fn emitted_header_queries_include_hidden_entries() {
    let bottom = layer(&[("etc/removed", Kind::Regular)]);
    let top = layer(&[("etc/.wh.removed", Kind::Regular)]);
    let selected = Selection::new([&bottom, &top]);
    assert_eq!(
        selected
            .names()
            .into_iter()
            .map(Name::as_str)
            .collect::<Vec<_>>(),
        ["etc/.wh.removed", "etc/removed"]
    );
}

#[test]
fn hardlink_chains_use_emitting_layer_and_target_mode() {
    let bottom = layer(&[("target", Kind::Symlink("wrong-layer".to_owned()))]);
    let top = Layer::parse([
        ("first".to_owned(), entry(hardlink("second"), 0o444)),
        ("second".to_owned(), entry(hardlink("/target"), 0o444)),
        ("target".to_owned(), entry(Kind::Regular, 0o755)),
    ])
    .unwrap();
    let selected = Selection::new([&bottom, &top]);
    let found = selected.lookup(&Name::parse("first").unwrap()).unwrap();
    let (name, entry) = found.regular().unwrap();
    assert_eq!(name.as_str(), "target");
    assert_eq!(entry.mode, 0o755);
}

#[test]
fn hardlink_cycles_missing_targets_and_nonregular_targets_fail() {
    let layer = layer(&[
        ("self", hardlink("self")),
        ("a", hardlink("b")),
        ("b", hardlink("a")),
        ("missing", hardlink("absent")),
        ("symlink", Kind::Symlink("absent".to_owned())),
        ("nonregular", hardlink("symlink")),
        ("regular", Kind::Regular),
    ]);
    let selected = Selection::new([&layer]);
    for name in ["self", "a", "b"] {
        assert!(matches!(
            selected
                .lookup(&Name::parse(name).unwrap())
                .unwrap()
                .regular(),
            Err(Error::HardlinkCycle(_))
        ));
    }
    assert!(matches!(
        selected
            .lookup(&Name::parse("missing").unwrap())
            .unwrap()
            .regular(),
        Err(Error::MissingTarget(_))
    ));
    for name in ["symlink", "nonregular"] {
        assert!(matches!(
            selected
                .lookup(&Name::parse(name).unwrap())
                .unwrap()
                .regular(),
            Err(Error::NotRegular(_))
        ));
    }
    assert!(
        selected
            .lookup(&Name::parse("regular").unwrap())
            .unwrap()
            .regular()
            .is_ok()
    );
}

#[test]
fn store_paths_come_only_from_selected_headers_and_are_sorted_unique() {
    let root = format!("nix/store/{}-fixture", "a".repeat(32));
    let bottom = layer(&[
        (&format!("{root}/bin/runner"), Kind::Regular),
        (&format!("{root}/share/data"), Kind::Regular),
    ]);
    let top = layer(&[("etc/no-store", Kind::Regular)]);
    assert_eq!(
        Selection::new([&bottom, &top]).store_paths(),
        BTreeSet::from([format!("/{root}")])
    );
    assert!(Selection::new([&top]).store_paths().is_empty());
    for invalid in [
        format!("notnix/store/{}-fixture", "a".repeat(32)),
        format!("nix/store/{}-", "a".repeat(32)),
        format!("nix/store/{}-fixture", "A".repeat(32)),
        format!("nix/store/{}-fixture", "a".repeat(31)),
    ] {
        assert!(store_path(&invalid).is_none());
    }
}
