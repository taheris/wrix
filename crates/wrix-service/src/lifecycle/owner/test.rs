use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

use super::*;
use crate::lifecycle::{CacheMode, Paths};
use wrix_core::path::Workspace;

fn selected(root: &Path) -> Selected {
    let workspace = Workspace::from_path(root).unwrap();
    Selected {
        runtime: RuntimeKind::Podman,
        workspace: workspace.hash().clone(),
        container: workspace.container_name(),
        port: NonZeroU16::new(21_345).unwrap(),
        recipe: None,
    }
}

#[test]
fn passive_lookup_does_not_create_host_state() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("host-lifecycle");
    assert_eq!(Selected::read(&missing).unwrap(), None);
    assert!(!missing.exists());
}

#[test]
fn selected_owner_is_durable_immutable_and_independent_of_other_roles() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let owner = selected(root.path());
    fs::write(host.join("other-role"), "retained role version").unwrap();
    owner.record(&guard).unwrap();
    assert_eq!(Selected::read(&host).unwrap(), Some(owner.clone()));
    let inode = fs::metadata(host.join(FILE)).unwrap().ino();
    owner.record(&guard).unwrap();
    assert_eq!(fs::metadata(host.join(FILE)).unwrap().ino(), inode);
    let mut competing = owner.clone();
    competing.runtime = RuntimeKind::Container;
    assert!(matches!(competing.record(&guard), Err(Error::Conflict)));
    assert_eq!(Selected::read(&host).unwrap(), Some(owner));
    assert_eq!(
        fs::read_to_string(host.join("other-role")).unwrap(),
        "retained role version"
    );
}

#[test]
fn selected_metadata_rejects_unsafe_files_without_repair() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let owner = selected(root.path());
    owner.record(&guard).unwrap();
    let target = host.join("retained");
    fs::rename(host.join(FILE), &target).unwrap();
    symlink(&target, host.join(FILE)).unwrap();
    assert!(Selected::read(&host).is_err());
    assert!(owner.record(&guard).is_err());
    fs::remove_file(host.join(FILE)).unwrap();
    fs::hard_link(&target, host.join(FILE)).unwrap();
    assert!(matches!(Selected::read(&host), Err(Error::Permissions)));
    fs::remove_file(host.join(FILE)).unwrap();
    fs::rename(&target, host.join(FILE)).unwrap();
    fs::set_permissions(host.join(FILE), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(Selected::read(&host), Err(Error::Permissions)));
}

fn recipe() -> Recipe {
    serde_json::from_value(serde_json::json!({
        "binding": {
            "schema": 1,
            "image": "localhost/wrix-service:selected",
            "source_kind": "nix-descriptor",
            "source": {"path": "/nix/store/source", "derivation": "/nix/store/source.drv"},
            "digest": {"path": "/nix/store/digest/value", "derivation": "/nix/store/digest.drv"}
        },
        "image_id": format!("sha256:{}", "a".repeat(64)),
        "config_digest": format!("sha256:{}", "b".repeat(64))
    }))
    .unwrap()
}

#[test]
fn new_owner_retains_exact_package_binding_and_native_identity() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let mut owner = selected(root.path());
    owner.recipe = Some(recipe());
    owner.record(&guard).unwrap();
    assert_eq!(Selected::read(&host).unwrap(), Some(owner.clone()));
    let bytes = fs::read(host.join(FILE)).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["schema_version"], 2);
    assert_eq!(
        wire["recipe"]["binding"]["image"],
        "localhost/wrix-service:selected"
    );
    let mut caller = owner;
    caller.recipe.as_mut().unwrap().config_digest =
        Digest::parse(&format!("sha256:{}", "c".repeat(64))).unwrap();
    assert!(matches!(caller.record(&guard), Err(Error::Conflict)));
    assert_eq!(fs::read(host.join(FILE)).unwrap(), bytes);
}

#[test]
fn legacy_owner_is_not_implicitly_assigned_the_callers_package() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let mut owner = selected(root.path());
    owner.record(&guard).unwrap();
    let before = fs::read(host.join(FILE)).unwrap();
    let inode = fs::metadata(host.join(FILE)).unwrap().ino();
    owner.recipe = Some(recipe());
    assert!(matches!(owner.record(&guard), Err(Error::Conflict)));
    assert_eq!(fs::read(host.join(FILE)).unwrap(), before);
    assert_eq!(fs::metadata(host.join(FILE)).unwrap().ino(), inode);
    assert_eq!(Selected::read(&host).unwrap().unwrap().recipe, None);
}

#[test]
fn frozen_owner_rejects_incomplete_or_unvalidated_provenance() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let mut owner = selected(root.path());
    owner.recipe = Some(recipe());
    owner.record(&guard).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join(FILE)).unwrap()).unwrap();
    for (path, value) in [
        ("/schema_version", serde_json::json!(1)),
        ("/recipe", serde_json::Value::Null),
        ("/recipe/image_id", serde_json::json!("opaque-id")),
        ("/recipe/config_digest", serde_json::json!("sha256:short")),
        ("/recipe/binding/schema", serde_json::json!(2)),
        (
            "/recipe/binding/source/path",
            serde_json::json!("/tmp/../escape"),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        fs::write(host.join(FILE), serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(Selected::read(&host).is_err(), "{path}");
    }
}

#[test]
fn oversized_frozen_record_is_rejected_before_creating_selection() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let mut owner = selected(root.path());
    let mut value = serde_json::to_value(recipe()).unwrap();
    value["binding"]["source"]["path"] =
        serde_json::json!(format!("/nix/store/{}", "x".repeat(4096)));
    owner.recipe = Some(serde_json::from_value(value).unwrap());
    assert!(matches!(owner.record(&guard), Err(Error::Invalid)));
    assert!(!host.join(FILE).exists());
}

#[test]
fn unknown_versions_and_fields_never_become_absent_selection() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    selected(root.path()).record(&guard).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(host.join(FILE)).unwrap()).unwrap();
    for (field, value) in [
        ("schema_version", serde_json::json!(2)),
        ("backend_version", serde_json::json!("unknown")),
        ("runtime", serde_json::json!("unknown")),
        ("workspace_hash", serde_json::json!("short")),
        ("container_name", serde_json::json!("invalid/name")),
        ("cache_http_port", serde_json::json!(0)),
        ("new_authority", serde_json::json!(true)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        fs::write(host.join(FILE), serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(Selected::read(&host).is_err(), "{field}");
    }
}

#[test]
fn namespace_rereads_selection_under_host_lock_and_does_not_acquire_its_cache_role() {
    let root = tempfile::tempdir().unwrap();
    let peer = root.path().join("peer");
    fs::create_dir(&peer).unwrap();
    let mut plan = Plan::for_workspace_with_paths(
        Workspace::from_path(&peer).unwrap(),
        CacheMode::Disabled,
        Paths::new(root.path().join("namespace"), root.path().join("cache")),
    )
    .unwrap();
    plan.cache_port = Some(21_999);
    assert!(plan.owns_cache());
    let host = root.path().join("host-lifecycle");
    let guard = coordination::acquire(&host).unwrap();
    let mut owner = selected(root.path());
    owner.runtime = plan.namespace_runtime;
    owner.record(&guard).unwrap();
    plan.reconcile_owner(&guard).unwrap();
    assert_eq!(plan.cache_port(), Some(owner.port.get()));
    assert_eq!(plan.cache_container_name(), owner.container);
    assert!(!plan.owns_cache());
    assert!(!plan.has_services());
    assert_eq!(plan.selected_host_ports(), Vec::<u16>::new());
    assert_eq!(plan.owned_cache_port(), None);
    fs::remove_file(host.join(FILE)).unwrap();
    assert!(plan.reconcile_owner(&guard).is_err());
}
