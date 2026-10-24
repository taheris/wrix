use super::*;
use std::os::unix::fs::PermissionsExt;
fn fixture() -> (tempfile::TempDir, Registry, Id, Vec<StorePath>) {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    let registry = Registry {
        records: root.join("records"),
        pins: root.join("pins"),
    };
    (
        directory,
        registry,
        "a".repeat(64).try_into().unwrap(),
        vec![
            "/nix/store/00000000000000000000000000000000-software"
                .to_owned()
                .try_into()
                .unwrap(),
        ],
    )
}
#[test]
fn delta_imports_must_be_unique_subsets_before_any_directory_is_published() {
    let (directory, registry, image, paths) = fixture();
    let extra: StorePath = "/nix/store/00000000000000000000000000000000-extra"
        .to_owned()
        .try_into()
        .unwrap();
    assert!(registry.begin_import(&image, &paths, &[extra]).is_err());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    let (directory, registry, image, paths) = fixture();
    assert!(
        registry
            .begin_import(&image, &paths, &[paths[0].clone(), paths[0].clone()])
            .is_err()
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}
#[test]
fn already_installed_paths_can_receive_exact_durable_pins_without_an_import_role() {
    let (_directory, registry, image, paths) = fixture();
    let records = registry.records.clone();
    let pins = registry.pins.clone();
    registry
        .begin_import(&image, &paths, &[])
        .unwrap()
        .finish()
        .unwrap();
    let registry = Registry { records, pins };
    assert_eq!(registry.inspect(&image, &paths).unwrap(), Status::Pinned);
    assert!(registry.importing(&image).is_err());
}
#[test]
fn missing_status_is_read_only_and_interrupted_import_is_never_missing_or_complete() {
    let (_directory, registry, image, paths) = fixture();
    assert_eq!(registry.inspect(&image, &paths).unwrap(), Status::Missing);
    assert!(!registry.records.exists());
    assert!(!registry.pins.exists());
    let records = registry.records.clone();
    let pins = registry.pins.clone();
    drop(registry.begin(&image, &paths).unwrap());
    let registry = Registry { records, pins };
    assert!(registry.inspect(&image, &paths).is_err());
    assert!(registry.begin(&image, &paths).is_err());
}
#[test]
fn complete_pins_are_exact_and_changed_missing_or_extra_links_are_not_repaired() {
    let (_directory, registry, image, paths) = fixture();
    let records = registry.records.clone();
    let pins = registry.pins.clone();
    registry.begin(&image, &paths).unwrap().finish().unwrap();
    let registry = Registry { records, pins };
    assert_eq!(registry.inspect(&image, &paths).unwrap(), Status::Pinned);
    let changed = vec![
        "/nix/store/00000000000000000000000000000000-other"
            .to_owned()
            .try_into()
            .unwrap(),
    ];
    assert!(registry.inspect(&image, &changed).is_err());
    let root = registry.pins.join(image.as_str());
    std::os::unix::fs::symlink(paths[0].as_str(), root.join("extra")).unwrap();
    assert!(registry.inspect(&image, &paths).is_err());
    fs::remove_file(root.join("extra")).unwrap();
    fs::remove_file(root.join(paths[0].basename())).unwrap();
    assert!(registry.inspect(&image, &paths).is_err());
}
#[test]
fn legacy_or_substituted_image_state_is_not_adopted() {
    let (_directory, registry, image, paths) = fixture();
    create(&registry.pins).unwrap();
    create(&registry.pins.join(image.as_str())).unwrap();
    assert!(registry.inspect(&image, &paths).is_err());
    assert!(registry.begin(&image, &paths).is_err());
    let (_directory, registry, image, paths) = fixture();
    let records = registry.records.clone();
    let pins = registry.pins.clone();
    registry.begin(&image, &paths).unwrap().finish().unwrap();
    let registry = Registry { records, pins };
    fs::hard_link(
        registry.records.join(image.as_str()),
        registry.records.join("alias"),
    )
    .unwrap();
    assert!(registry.inspect(&image, &paths).is_err());
}
