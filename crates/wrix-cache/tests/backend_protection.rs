use std::path::{Path, PathBuf};
use wrix_cache::backend::publication::{
    Publisher,
    protection::{Failure, Protection},
    workflow::realization::{self, Installable},
};

#[test]
fn protection_rejects_derivation_realization_and_nonstore_paths_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    for path in [
        "/workspace/target",
        "/nix/store/not-an-output.drv",
        "/nix/store/a/nested",
    ] {
        assert!(matches!(
            Protection::acquire(directory.path(), &[PathBuf::from(path)]),
            Err(Failure::Path)
        ));
    }
    assert!(!directory.path().join("publication-roots").exists());
    assert!(Installable::parse("--no-link".into()).is_err());
    assert!(Installable::parse(String::new()).is_err());
}

#[test]
#[ignore = "requires WRIX_TEST_STORE_OUTPUT naming an already-realized native host Nix output"]
fn native_roots_survive_caller_loss_and_release_only_after_explicit_completion() {
    let artifact =
        std::env::var_os("WRIX_TEST_STORE_OUTPUT").expect("native realized output required");
    let artifact = Path::new(&artifact);
    let directory = tempfile::tempdir().unwrap();
    let protected = Protection::acquire(directory.path(), &[artifact.to_path_buf()]).unwrap();
    let root = protected.directory().to_path_buf();
    assert_eq!(std::fs::canonicalize(root.join("0")).unwrap(), artifact);
    drop(protected);
    assert!(
        root.join("0").exists(),
        "caller loss cannot release uncertain protection"
    );
    let completed = Protection::acquire(directory.path(), &[artifact.to_path_buf()]).unwrap();
    let completed_root = completed.directory().to_path_buf();
    completed.release().unwrap();
    assert!(!completed_root.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires WRIX_TEST_STORE_OUTPUT naming an already-realized native host Nix output"]
fn selected_realization_hands_off_outlinks_before_native_protection_ends() {
    let artifact =
        PathBuf::from(std::env::var_os("WRIX_TEST_STORE_OUTPUT").expect("native output required"));
    let directory = tempfile::tempdir().unwrap();
    let control = directory.path().join("control");
    let workspace = wrix_core::path::Workspace::from_path(directory.path()).unwrap();
    Publisher::install(
        &control,
        workspace.hash(),
        "http://127.0.0.1:1/",
        attic::cache::CacheName::new("private".into()).unwrap(),
        "fixture",
        std::time::Duration::from_secs(3600),
    )
    .unwrap();
    let publisher = Publisher::load(&control, workspace.hash()).unwrap();
    let selected = Installable::parse(artifact.to_str().unwrap().to_owned()).unwrap();
    let realized = realization::realize(&publisher, directory.path(), &[selected]).unwrap();
    assert_eq!(realized.paths(), [artifact]);
    let origin = realized.directory().to_path_buf();
    assert!(origin.join("result").exists());
    let candidates = realized.paths().to_vec();
    let protected = realized.handoff(&publisher, &candidates).unwrap();
    assert!(!origin.exists());
    let roots = std::fs::read_dir(control.join("publication-roots"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(roots.len(), 1);
    assert!(roots[0].path().join("0").exists());
    protected.discard().unwrap();
    assert!(!roots[0].path().exists());
}
