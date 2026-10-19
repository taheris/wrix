use std::{
    fs,
    num::NonZeroU64,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    sync::{Arc, Barrier},
};

use serde_json::{Value, json};
use wrix_sandbox::image::{Digest, ImageRef};
use wrix_service::{
    control::Controller,
    deployment::{
        Generation, Id,
        selection::{Record, Role},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn image(name: &str) -> Result<ImageRef, wrix_sandbox::image::ImageRefParseError> {
    ImageRef::parse(name)
}

fn digest(byte: char) -> Result<Digest, wrix_sandbox::image::DigestParseError> {
    Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn private_json(path: &Path, value: &Value) -> TestResult {
    fs::write(path, serde_json::to_vec(value)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[tokio::test]
async fn bootstrap_records_every_role_without_namespace_or_runtime_effects() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    assert_eq!(controller.recorded_selection()?, None);
    assert!(!root.join("selection.json").exists());
    let selected = controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    assert_eq!(selected.deployment(), controller.deployment_id());
    for role in Role::ALL {
        let target = selected.target(role);
        assert_eq!(target.generation(), Generation::new(NonZeroU64::MIN));
        assert_eq!(target.image(), &image("service:blue")?);
        assert_eq!(target.digest(), &digest('a')?);
    }
    assert_eq!(controller.recorded_selection()?, Some(selected));
    assert_eq!(
        fs::metadata(root.join("selection.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!root.join("namespaces.json").exists());
    assert!(!root.join("role-endpoints.json").exists());
    assert!(!root.join("service-data").exists());
    controller.close()?;
    Ok(())
}

#[tokio::test]
async fn ordinary_bootstrap_resumes_recorded_versions_after_host_stop() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let selected = controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    let before = fs::read(root.join("selection.json"))?;
    controller.close()?;
    let resumed = Controller::bind(&root)?;
    assert_eq!(resumed.recorded_selection()?, Some(selected.clone()));
    assert_eq!(
        resumed.bootstrap_selection(image("service:green")?, digest('b')?)?,
        selected
    );
    assert_eq!(fs::read(root.join("selection.json"))?, before);
    resumed.close()?;
    Ok(())
}

#[tokio::test]
async fn concurrent_bootstrap_has_one_selection_even_with_different_callers() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Arc::new(Controller::bind(&root)?);
    let barrier = Arc::new(Barrier::new(2));
    let attempt = |image, digest| {
        let controller = controller.clone();
        let barrier = barrier.clone();
        tokio::task::spawn_blocking(move || {
            barrier.wait();
            controller.bootstrap_selection(image, digest)
        })
    };
    let (first, second) = tokio::try_join!(
        attempt(image("service:blue")?, digest('a')?),
        attempt(image("service:green")?, digest('b')?)
    )?;
    assert_eq!(first?, second?);
    let persisted: Value = serde_json::from_slice(&fs::read(root.join("selection.json"))?)?;
    let selected: Record = serde_json::from_value(persisted["selection"].clone())?;
    assert_eq!(controller.recorded_selection()?, Some(selected));
    drop(controller);
    Ok(())
}

#[tokio::test]
async fn failed_initial_commit_does_not_select_or_overwrite_existing_state() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let path = root.join("selection.json");
    fs::write(&path, b"unreconciled")?;
    assert!(
        controller
            .bootstrap_selection(image("service:blue")?, digest('a')?)
            .is_err()
    );
    assert_eq!(controller.recorded_selection()?, None);
    assert_eq!(fs::read(&path)?, b"unreconciled");
    fs::remove_file(path)?;
    controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    assert!(controller.recorded_selection()?.is_some());
    controller.close()?;
    Ok(())
}

#[tokio::test]
async fn restart_preserves_each_backend_assignment_after_partial_selection() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    controller.close()?;
    let path = root.join("selection.json");
    let mut snapshot: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let green = json!({"generation": 2, "image": "service:green", "digest": digest('b')?});
    snapshot["selection"]["gateway"] = green.clone();
    snapshot["selection"]["dolt"] = green;
    private_json(&path, &snapshot)?;
    let before = fs::read(&path)?;
    let controller = Controller::bind(&root)?;
    let record = controller.bootstrap_selection(image("service:newer")?, digest('c')?)?;
    assert_eq!(record.target(Role::Gateway).generation().number().get(), 2);
    assert_eq!(record.target(Role::Dolt).digest(), &digest('b')?);
    assert_eq!(record.target(Role::Celler).generation().number().get(), 1);
    assert_eq!(record.target(Role::Compiler).digest(), &digest('a')?);
    assert_eq!(fs::read(&path)?, before);
    assert!(!root.join("role-endpoints.json").exists());
    controller.close()?;
    Ok(())
}

#[tokio::test]
async fn malformed_selection_fails_before_host_authority_publication() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    controller.close()?;
    let path = root.join("selection.json");
    let valid: Value = serde_json::from_slice(&fs::read(&path)?)?;
    fs::remove_file(root.join("management.key"))?;
    let mut invalid = Vec::new();
    let mut value = valid.clone();
    value["version"] = json!(2);
    invalid.push(value);
    let mut value = valid.clone();
    value["selection"]["deployment"] = json!(Id::try_from("f".repeat(32))?);
    invalid.push(value);
    let mut value = valid.clone();
    value["selection"]
        .as_object_mut()
        .unwrap()
        .remove("compiler");
    invalid.push(value);
    let mut value = valid.clone();
    value["selection"]["gateway"]["generation"] = json!(0);
    invalid.push(value);
    let mut value = valid.clone();
    value["selection"]["dolt"]["digest"] = json!(digest('b')?);
    invalid.push(value);
    let mut value = valid.clone();
    value["selection"]["celler"]["image"] = json!("other:tag");
    invalid.push(value);
    let mut value = valid;
    value["selection"]["ready"] = json!(true);
    invalid.push(value);
    for value in invalid {
        private_json(&path, &value)?;
        let before = fs::read(&path)?;
        assert!(Controller::bind(&root).is_err(), "{value}");
        assert_eq!(fs::read(&path)?, before);
        assert!(!root.join("management.key").exists());
        assert!(!root.join("control.sock").exists());
    }
    for bytes in [b"{".as_slice(), b"", &[b'a'; 4097]] {
        fs::write(&path, bytes)?;
        assert!(Controller::bind(&root).is_err());
        assert_eq!(fs::read(&path)?, bytes);
        assert!(!root.join("management.key").exists());
    }
    Ok(())
}

#[tokio::test]
async fn orphaned_selection_cannot_create_a_replacement_deployment_identity() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    controller.close()?;
    fs::remove_file(root.join("deployment.json"))?;
    fs::remove_file(root.join("management.key"))?;
    let before = fs::read(root.join("selection.json"))?;
    assert!(Controller::bind(&root).is_err());
    assert!(!root.join("deployment.json").exists());
    assert!(!root.join("management.key").exists());
    assert_eq!(fs::read(root.join("selection.json"))?, before);
    Ok(())
}

#[tokio::test]
async fn selection_rejects_symlink_and_nonprivate_files_without_following_them() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    controller.bootstrap_selection(image("service:blue")?, digest('a')?)?;
    controller.close()?;
    let path = root.join("selection.json");
    let outside = directory.path().join("outside.json");
    fs::rename(&path, &outside)?;
    let before = fs::read(&outside)?;
    symlink(&outside, &path)?;
    assert!(Controller::bind(&root).is_err());
    assert_eq!(fs::read(&outside)?, before);
    fs::remove_file(&path)?;
    fs::rename(&outside, &path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(Controller::bind(&root).is_err());
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}
