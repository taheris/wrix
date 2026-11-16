use super::*;

#[test]
fn registration_names_cross_batch_boundaries_without_collisions() {
    let desired: BTreeSet<_> = (0..260)
        .map(|index| StorePath::parse(&format!("/nix/store/{index:032}-fixture")).unwrap())
        .collect();
    let indexed = indexed(&desired);
    assert_eq!(indexed.len(), desired.len());
    assert_eq!(
        indexed["batch-0"],
        StorePath::parse("/nix/store/00000000000000000000000000000000-fixture").unwrap()
    );
    assert_eq!(
        indexed["batch-0-128"],
        StorePath::parse("/nix/store/00000000000000000000000000000127-fixture").unwrap()
    );
    assert_eq!(
        indexed["batch-1"],
        StorePath::parse("/nix/store/00000000000000000000000000000128-fixture").unwrap()
    );
    assert_eq!(
        indexed["batch-2-4"],
        StorePath::parse("/nix/store/00000000000000000000000000000259-fixture").unwrap()
    );
}

#[test]
fn mismatched_indexed_link_is_refused_before_any_retirement() {
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("cache");
    initialize(&directory).unwrap();
    let generation = directory.join("generation-modified");
    fs::create_dir(&generation).unwrap();
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        generation.join("manifest"),
        "/nix/store/00000000000000000000000000000000-first\n",
    )
    .unwrap();
    symlink(
        "/nix/store/11111111111111111111111111111111-second",
        generation.join("batch-0"),
    )
    .unwrap();
    let cache = Cache::open(&directory).unwrap();
    assert!(
        cache
            .replace(&BTreeSet::new(), Path::new("unused-nix"), &mut Vec::new())
            .is_err()
    );
    assert!(generation.join("manifest").exists());
    assert!(generation.join("batch-0").is_symlink());
}
