use super::{Candidate, Error};
use serde_json::json;
use std::{ffi::OsString, fs};

fn binding() -> serde_json::Value {
    json!({
        "schema": 1,
        "image": "localhost/wrix-nix-pool:pinned",
        "source_kind": "nix-descriptor",
        "source": {"path":"/nix/store/pool-source", "derivation":"/nix/store/pool-source.drv"},
        "digest": {"path":"/nix/store/pool-layout/config-digest", "derivation":"/nix/store/pool-layout.drv"}
    })
}

#[test]
fn pool_selection_never_falls_back_to_core_service_or_project_images() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pool.json");
    fs::write(&path, serde_json::to_vec(&binding()).unwrap()).unwrap();
    let selected = Candidate::resolve(Some(&path), |_| None).unwrap();
    let ambient = |name: &str| match name {
        "WRIX_SERVICE_PACKAGE_BINDING" | "WRIX_SERVICE_IMAGE" | "WRIX_PROFILE_CONFIG" | "PWD" => {
            Some(OsString::from("unrelated-project-or-core-image"))
        }
        _ => None,
    };
    assert_eq!(selected, Candidate::resolve(Some(&path), ambient).unwrap());
    assert!(matches!(
        Candidate::resolve(None, ambient),
        Err(Error::Missing)
    ));
    assert_eq!(selected.image().as_str(), "localhost/wrix-nix-pool:pinned");
    assert_eq!(
        selected.source().path(),
        std::path::Path::new("/nix/store/pool-source")
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn raw_pool_image_environment_cannot_override_even_a_valid_binding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pool.json");
    fs::write(&path, serde_json::to_vec(&binding()).unwrap()).unwrap();
    for selector in [
        "WRIX_NIX_POOL_IMAGE",
        "WRIX_NIX_POOL_IMAGE_SOURCE",
        "WRIX_NIX_POOL_IMAGE_SOURCE_KIND",
        "WRIX_NIX_POOL_IMAGE_DIGEST",
    ] {
        let result = Candidate::resolve(Some(&path), |name| {
            (name == selector).then(|| "override".into())
        });
        assert!(matches!(result, Err(Error::Selector(name)) if name == selector));
    }
}

#[test]
fn invalid_pool_binding_does_not_become_a_core_service_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pool.json");
    for (field, invalid) in [
        ("schema", json!(2)),
        ("source_kind", json!("unknown")),
        (
            "source",
            json!({"path":"relative", "derivation":"/nix/store/pool.drv"}),
        ),
        ("project_override", json!(true)),
    ] {
        let mut value = binding();
        value[field] = invalid;
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            Candidate::resolve(Some(&path), |_| None),
            Err(Error::Binding(_))
        ));
    }
}
