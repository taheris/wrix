use super::*;

const A: &str = "/nix/store/00000000000000000000000000000000-a";
const B: &str = "/nix/store/11111111111111111111111111111111-b";

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("cache");
    initialize(&directory).unwrap();
    (parent, directory)
}

fn generation(directory: &Path, name: &str, target: &str) -> PathBuf {
    let result = directory.join(name);
    fs::create_dir(&result).unwrap();
    fs::set_permissions(&result, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(target, result.join(Path::new(target).file_name().unwrap())).unwrap();
    result
}

#[test]
fn refuses_existing_foreign_directory_without_modification() {
    let parent = tempfile::tempdir().unwrap();
    fs::write(parent.path().join("precious"), "keep").unwrap();
    assert!(initialize(parent.path()).is_err());
    assert!(Cache::open(parent.path()).is_err());
    assert_eq!(
        fs::read_to_string(parent.path().join("precious")).unwrap(),
        "keep"
    );
}

#[test]
fn store_paths_reject_descendants_traversal_and_newlines() {
    for bad in [
        "/tmp/a",
        "/nix/store/a",
        "/nix/store/00000000000000000000000000000000-a/file",
        "/nix/store/00000000000000000000000000000000-../x",
        "/nix/store/00000000000000000000000000000000-a\n",
    ] {
        assert!(StorePath::parse(bad).is_err(), "{bad}");
    }
    assert!(StorePath::parse(A).is_ok());
}

#[test]
fn concurrent_owner_is_refused_until_lock_is_released() {
    let (_parent, directory) = fixture();
    let owner = Cache::open(&directory).unwrap();
    assert!(matches!(Cache::open(&directory), Err(Error::Busy)));
    drop(owner);
    assert!(Cache::open(&directory).is_ok());
}

#[test]
fn dry_run_preserves_current_and_abandoned_roots() {
    let (_parent, directory) = fixture();
    let old = generation(&directory, "generation-old", A);
    generation(&directory, "generation-orphan", B);
    symlink("generation-old", directory.join("current")).unwrap();
    let cache = Cache::open(&directory).unwrap();
    let mut output = Vec::new();
    cache
        .plan(&BTreeSet::from([StorePath::parse(A).unwrap()]), &mut output)
        .unwrap();
    assert!(String::from_utf8(output).unwrap().contains("retire\t"));
    assert_eq!(fs::read_dir(old).unwrap().count(), 1);
    assert!(directory.join("generation-orphan").is_dir());
}

#[test]
fn replacement_recovers_abandoned_generations_without_touching_targets() {
    let (_parent, directory) = fixture();
    generation(&directory, "generation-old", A);
    generation(&directory, "generation-orphan", B);
    symlink("generation-old", directory.join("current")).unwrap();
    symlink("generation-orphan", directory.join("next")).unwrap();
    let cache = Cache::open(&directory).unwrap();
    cache
        .replace(&BTreeSet::new(), Path::new("unused-nix"), &mut Vec::new())
        .unwrap();
    assert_eq!(cache.generations().unwrap().len(), 1);
    let current = fs::read_link(directory.join("current")).unwrap();
    assert!(directory.join(current).is_dir());
    assert!(!directory.join("generation-old").exists());
    assert!(!directory.join("generation-orphan").exists());
}

#[test]
fn changed_link_refuses_entire_retirement_before_removing_any_root() {
    let (_parent, directory) = fixture();
    let old = generation(&directory, "generation-old", A);
    let root = old.join(Path::new(A).file_name().unwrap());
    fs::remove_file(&root).unwrap();
    symlink(B, &root).unwrap();
    let cache = Cache::open(&directory).unwrap();
    assert!(
        cache
            .replace(&BTreeSet::new(), Path::new("unused-nix"), &mut Vec::new())
            .is_err()
    );
    assert_eq!(fs::read_link(&root).unwrap(), Path::new(B));
}

#[test]
fn symlinked_generation_cannot_reach_foreign_roots() {
    let (parent, directory) = fixture();
    let foreign = parent.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), "data").unwrap();
    symlink(&foreign, directory.join("generation-foreign")).unwrap();
    let cache = Cache::open(&directory).unwrap();
    assert!(
        cache
            .replace(&BTreeSet::new(), Path::new("unused-nix"), &mut Vec::new())
            .is_err()
    );
    assert_eq!(fs::read_to_string(foreign.join("keep")).unwrap(), "data");
}

#[test]
fn failed_registration_preserves_current_and_bounds_pending_generation() {
    let (_parent, directory) = fixture();
    let old = generation(&directory, "generation-old", A);
    symlink("generation-old", directory.join("current")).unwrap();
    let cache = Cache::open(&directory).unwrap();
    for _ in 0..4 {
        assert!(matches!(
            cache.replace(
                &BTreeSet::from([StorePath::parse(B).unwrap()]),
                Path::new("false"),
                &mut Vec::new()
            ),
            Err(Error::Nix)
        ));
        assert_eq!(cache.generations().unwrap().len(), 2);
        assert_eq!(fs::read_dir(&old).unwrap().count(), 1);
    }
}
