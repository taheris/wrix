use super::*;
use std::os::unix::fs::symlink;
use wrix_core::path::Workspace;

fn fixture(directory: &Path) -> (coordination::Guard, PathBuf, WorkspaceHash) {
    let workspace = Workspace::from_path(directory).unwrap();
    let hash = workspace.hash().clone();
    let root = directory.join("workspaces").join(hash.as_str());
    wrix_core::private_directory::prepare_with_parents(&root).unwrap();
    let guard = coordination::acquire(&directory.join("host-lifecycle")).unwrap();
    (guard, root, hash)
}

#[test]
fn passive_missing_lookup_does_not_create_or_repair_state() {
    let directory = tempfile::tempdir().unwrap();
    let hash = Workspace::from_path(directory.path())
        .unwrap()
        .hash()
        .clone();
    let root = directory.path().join("missing");
    ensure_clear(&root, &hash).unwrap();
    assert!(!root.exists());
}

#[test]
fn lost_caller_preserves_uncertainty_and_forbids_replay_or_resume() {
    let directory = tempfile::tempdir().unwrap();
    let (guard, root, hash) = fixture(directory.path());
    let pending = Pending::begin(&guard, &root, &hash, Change::Retire).unwrap();
    let before = fs::read(root.join(FILE)).unwrap();
    drop(pending);
    assert!(matches!(ensure_clear(&root, &hash), Err(Error::Pending)));
    assert!(matches!(
        Pending::begin(&guard, &root, &hash, Change::Resume),
        Err(Error::Pending)
    ));
    assert_eq!(fs::read(root.join(FILE)).unwrap(), before);
}

#[test]
fn only_matching_completed_observation_clears_the_fence() {
    let directory = tempfile::tempdir().unwrap();
    let (guard, root, hash) = fixture(directory.path());
    Pending::begin(&guard, &root, &hash, Change::Retire)
        .unwrap()
        .finish(Observation::Retired)
        .unwrap();
    ensure_clear(&root, &hash).unwrap();
    let pending = Pending::begin(&guard, &root, &hash, Change::Resume).unwrap();
    assert!(matches!(
        pending.finish(Observation::Retired),
        Err(Error::Pending)
    ));
    assert!(matches!(ensure_clear(&root, &hash), Err(Error::Pending)));
}

#[test]
fn foreign_guard_and_unsafe_marker_are_rejected_without_repair() {
    let directory = tempfile::tempdir().unwrap();
    let (_guard, root, hash) = fixture(directory.path());
    let other = tempfile::tempdir().unwrap();
    let guard = coordination::acquire(&other.path().join("host-lifecycle")).unwrap();
    assert!(matches!(
        Pending::begin(&guard, &root, &hash, Change::Retire),
        Err(Error::Incompatible)
    ));
    let target = directory.path().join("preserve");
    fs::write(&target, "preserved").unwrap();
    symlink(&target, root.join(FILE)).unwrap();
    assert!(ensure_clear(&root, &hash).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "preserved");
}
