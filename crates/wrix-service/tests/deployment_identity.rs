use std::{
    fs,
    num::NonZeroU64,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use serde_json::json;
use wrix_core::path::Workspace;
use wrix_service::{
    control::{Controller, Namespace},
    deployment::{Backend, Generation, Id, Layout},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn controller_identity_survives_restart_without_becoming_a_namespace() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let first = Controller::bind(&root)?;
    let id = first.deployment_id().clone();
    let snapshot = fs::read(root.join("deployment.json"))?;
    assert_eq!(
        fs::metadata(root.join("deployment.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(Controller::bind(&root).is_err());
    assert_eq!(fs::read(root.join("deployment.json"))?, snapshot);
    drop(first);
    fs::remove_file(root.join("control.sock"))?;
    let resumed = Controller::bind(&root)?;
    assert_eq!(resumed.deployment_id(), &id);
    assert_eq!(fs::read(root.join("deployment.json"))?, snapshot);
    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("namespaces.json"))?)?;
    assert_eq!(registry["version"], 2);
    assert_eq!(registry["entries"], serde_json::json!([]));
    let other = Controller::bind(&directory.path().canonicalize()?.join("other"))?;
    assert_ne!(other.deployment_id(), &id);
    Ok(())
}

#[tokio::test]
async fn concurrent_controller_ownership_creates_only_one_deployment_identity() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let attempt = |root: std::path::PathBuf| {
        let barrier = barrier.clone();
        tokio::task::spawn_blocking(move || {
            barrier.wait();
            Controller::bind(&root)
        })
    };
    let first = attempt(root.clone());
    let second = attempt(root.clone());
    let (first, second) = tokio::try_join!(first, second)?;
    let ((Ok(winner), Err(_)) | (Err(_), Ok(winner))) = (first, second) else {
        panic!("exclusive host ownership must admit exactly one controller");
    };
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("deployment.json"))?)?;
    assert_eq!(snapshot["id"], json!(winner.deployment_id()));
    assert_eq!(
        fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?,
        [
            "ownership",
            "deployment.json",
            "management.key",
            "control.sock",
            "namespaces.lock",
            "namespaces.json",
            "native-epoch"
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect()
    );
    Ok(())
}

#[tokio::test]
async fn partial_deployment_record_is_not_reinitialized() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let path = root.join("deployment.json");
    for bytes in [b"{".as_slice(), b"", &[b'a'; 257]] {
        fs::write(&path, bytes)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        assert!(Controller::bind(&root).is_err());
        assert_eq!(fs::read(&path)?, bytes);
        assert!(!root.join("management.key").exists());
        assert!(!root.join("control.sock").exists());
    }
    Ok(())
}

#[tokio::test]
async fn malformed_deployment_identity_fails_before_credential_or_socket_creation() -> TestResult {
    for value in [
        json!({"version": 2, "id": "a".repeat(32)}),
        json!({"version": 1, "id": "A".repeat(32)}),
        json!({"version": 1, "id": "../escape"}),
        json!({"version": 1, "id": "a".repeat(32), "selected": "untrusted"}),
        json!({"version": 1}),
    ] {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?.join("controller");
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let path = root.join("deployment.json");
        let before = serde_json::to_vec(&value)?;
        fs::write(&path, &before)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        assert!(Controller::bind(&root).is_err(), "{value}");
        assert_eq!(fs::read(path)?, before);
        assert!(!root.join("management.key").exists());
        assert!(!root.join("control.sock").exists());
    }
    Ok(())
}

#[tokio::test]
async fn deployment_identity_rejects_symlink_and_nonprivate_files() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let outside = directory.path().join("outside.json");
    let bytes = serde_json::to_vec(&json!({"version": 1, "id": "b".repeat(32)}))?;
    fs::write(&outside, &bytes)?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
    let path = root.join("deployment.json");
    symlink(&outside, &path)?;
    assert!(Controller::bind(&root).is_err());
    assert_eq!(fs::read(&outside)?, bytes);
    fs::remove_file(&path)?;
    fs::write(&path, &bytes)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(Controller::bind(&root).is_err());
    assert!(!root.join("management.key").exists());
    assert!(!root.join("control.sock").exists());
    Ok(())
}

#[test]
fn namespace_layout_is_generation_independent_and_separates_private_authority() -> TestResult {
    let directory = tempfile::tempdir()?;
    let repo = directory.path().join("repo");
    fs::create_dir_all(repo.join(".git"))?;
    let clone = repo.join(".loom/beads/issue");
    fs::create_dir_all(clone.join(".git"))?;
    let outer = Workspace::from_service_path(&repo)?;
    let bead = Workspace::from_service_path(&clone)?;
    let peer_path = directory.path().join("peer");
    fs::create_dir(&peer_path)?;
    let peer = Workspace::from_service_path(peer_path)?;
    for (state, cache) in [
        ("/home/user/.local/state", "/home/user/.cache"),
        (
            "/Users/user/Library/Application Support",
            "/Users/user/Library/Caches",
        ),
    ] {
        let layout = Layout::new(Path::new(state), Path::new(cache))?;
        let selected = layout.workspace(Namespace::from(outer.hash()));
        assert_ne!(selected, layout.workspace(Namespace::from(bead.hash())));
        let peer = layout.workspace(Namespace::from(peer.hash()));
        assert_ne!(selected.namespace(), peer.namespace());
        assert_ne!(selected.control_root(), peer.control_root());
        assert_ne!(selected.data_root(), peer.data_root());
        assert_ne!(selected.cache_lock_path(), peer.cache_lock_path());
        assert_ne!(selected.sync_lock_path(), peer.sync_lock_path());
        assert_eq!(
            selected.services_path(),
            selected.control_root().join("services.json")
        );
        assert_eq!(selected.dolt_root(), selected.data_root().join("dolt"));
        assert_eq!(
            selected.cache_root(),
            Path::new(cache)
                .join("wrix/workspaces")
                .join(outer.hash().as_str())
                .join("binary-cache")
        );
        for backend in [Backend::Dolt, Backend::Celler, Backend::Compiler] {
            let data = layout.backend_root(backend);
            assert!(!layout.control_root().starts_with(&data));
            assert!(!selected.control_root().starts_with(&data));
            assert!(!selected.data_root().starts_with(&data));
        }
        assert_ne!(
            layout.backend_root(Backend::Dolt),
            layout.backend_root(Backend::Celler)
        );
        assert_ne!(
            layout.backend_root(Backend::Celler),
            layout.backend_root(Backend::Compiler)
        );
        let blue = Generation::new(NonZeroU64::new(1).unwrap());
        let green = Generation::new(NonZeroU64::new(2).unwrap());
        assert_ne!(layout.generation_root(blue), layout.generation_root(green));
        assert!(
            layout
                .generation_root(blue)
                .starts_with(layout.control_root())
        );
        assert_eq!(selected, layout.workspace(Namespace::from(outer.hash())));
    }
    assert!(!repo.join(".wrix").exists());
    Ok(())
}

#[test]
fn deployment_boundaries_reject_invalid_identity_generation_and_paths() -> TestResult {
    for value in [
        "",
        "../escape",
        &"A".repeat(32),
        &"a".repeat(31),
        &"a".repeat(33),
    ] {
        assert!(Id::try_from(value.to_owned()).is_err());
        assert!(serde_json::from_value::<Id>(json!(value)).is_err());
    }
    let id = Id::try_from("a".repeat(32))?;
    assert_eq!(
        serde_json::from_value::<Id>(serde_json::to_value(&id)?)?,
        id
    );
    assert!(serde_json::from_value::<Generation>(json!(0)).is_err());
    assert!(serde_json::from_value::<Generation>(json!(-1)).is_err());
    for base in [
        "relative",
        "/tmp/../elsewhere",
        "/tmp/./state",
        "/tmp/bad\0path",
    ] {
        assert!(Layout::new(Path::new(base), Path::new("/cache")).is_err());
        assert!(Layout::new(Path::new("/state"), Path::new(base)).is_err());
    }
    Ok(())
}
