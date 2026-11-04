use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[path = "../../../../wrix-cli/tests/workspace/isolation.rs"]
mod isolation;

const INSPECT: &[u8] = include_bytes!("../../../tests/fixtures/apple-service-image/inspect.json");
const INDEX: &[u8] =
    include_bytes!("../../../tests/fixtures/apple-service-image/native-index.blob");
const CONFIG: &[u8] =
    include_bytes!("../../../tests/fixtures/apple-service-image/native-config.blob");

#[test]
fn apple_1_5_native_producer_keeps_index_manifest_and_config_identities_distinct() {
    let inspected: Value = serde_json::from_slice(INSPECT).unwrap();
    let native = index(&inspected).unwrap();
    require_exported_index(INDEX, &native).unwrap();
    let configuration = config_digest(CONFIG).unwrap();
    assert_eq!(
        configuration.as_str(),
        "sha256:167ed5405630356632ef31e8421fb0494a2804f2f47bfe35874d2909efcf549d"
    );
    let manifest = Digest::parse(inspected[0]["variants"][0]["digest"].as_str().unwrap()).unwrap();
    assert_ne!(native, manifest);
    assert_ne!(native, configuration);
    assert_ne!(manifest, configuration);
    let raw_index: Value = serde_json::from_slice(INDEX).unwrap();
    assert_eq!(
        raw_index["manifests"][0]["digest"],
        json!(manifest.as_str())
    );
    assert_eq!(
        inspected[0]["variants"][0]["config"],
        serde_json::from_slice::<Value>(CONFIG).unwrap()
    );
}

#[test]
fn a_restored_reference_cannot_attest_an_archive_from_the_intervening_index() {
    let inspected: Value = serde_json::from_slice(INSPECT).unwrap();
    let expected = index(&inspected).unwrap();
    // Both reference observations can honestly report A even though save read B.
    require_id(INSPECT, &ImageId::parse(expected.as_str()).unwrap()).unwrap();
    let mut intervening: Value = serde_json::from_slice(INDEX).unwrap();
    intervening["annotations"]["fixture_intervening_image"] = json!("B");
    let foreign = serde_json::to_vec(&intervening).unwrap();
    assert!(matches!(
        require_exported_index(&foreign, &expected),
        Err(Error::InvalidMetadata)
    ));
    require_id(INSPECT, &ImageId::parse(expected.as_str()).unwrap()).unwrap();
    require_exported_index(INDEX, &expected).unwrap();
}

#[test]
fn attestation_refuses_an_intervening_saved_image_before_writing_any_receipt() {
    const CASE: &str = "WRIX_TEST_APPLE_ATTEST_CASE";
    if let Ok(case) = std::env::var(CASE) {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let temporary = home.join("temporary");
        fs::create_dir(&temporary).unwrap();
        let result = attest(&ImageRef::parse("wrix-selected:test").unwrap(), &temporary);
        let inspected: Value = serde_json::from_slice(INSPECT).unwrap();
        if case == "intervening" {
            assert!(matches!(result, Err(Error::InvalidMetadata)));
            let host = wrix_state::host::Host::discover().unwrap();
            assert!(!host.roots().state().join("image").exists());
        } else {
            result.unwrap();
            assert_eq!(
                cached(&inspected).unwrap(),
                Some(config_digest(CONFIG).unwrap())
            );
        }
        return;
    }
    for case in ["intervening", "matching"] {
        let executable = std::env::current_exe().unwrap();
        let (root, isolated) = isolation::root(&executable);
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        if isolated {
            isolation::install_executable(root.path(), Path::new("/bin/sh"), Path::new("/bin/sh"));
        }
        let quote = |bytes: &[u8]| {
            format!(
                "'{}'",
                String::from_utf8_lossy(bytes).replace('\'', "'\\''")
            )
        };
        let container = format!(
            "#!/bin/sh\nset -eu\ncase \"$1 $2\" in\n'image inspect') printf '%s' {};;\n'image save') :;;\n*) exit 91;;\nesac\n",
            quote(INSPECT),
        );
        let mut saved: Value = serde_json::from_slice(INDEX).unwrap();
        if case == "intervening" {
            saved["annotations"]["fixture_intervening_image"] = json!("B");
        }
        let saved = if case == "matching" {
            INDEX.to_vec()
        } else {
            serde_json::to_vec(&saved).unwrap()
        };
        let skopeo = format!(
            "#!/bin/sh\nset -eu\ncase \" $* \" in\n*' --config '*) printf '%s' {};;\n*) printf '%s' {};;\nesac\n",
            quote(CONFIG),
            quote(&saved),
        );
        for (name, contents) in [("container", container), ("skopeo", skopeo)] {
            let path = bin.join(name);
            fs::write(&path, contents).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let child = if isolated {
            Path::new("/bin/wrix")
        } else {
            &executable
        };
        let thread = std::thread::current();
        let name = thread.name().unwrap();
        let path = if isolated {
            std::ffi::OsString::from("/bin")
        } else {
            std::env::join_paths(
                std::iter::once(bin)
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap()
        };
        let output = isolation::command(
            root.path(),
            isolated,
            child,
            &["--exact", name, "--nocapture"],
        )
        .env(CASE, case)
        .env("PATH", path)
        .output()
        .unwrap();
        assert!(output.status.success(), "{case}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[test]
fn historical_v1_receipts_are_not_upgraded_into_archive_bound_proof() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let native = index(&serde_json::from_slice(INSPECT).unwrap()).unwrap();
    let bytes = serde_json::to_vec(&Receipt {
        version: 1,
        index: native.clone(),
        config: config_digest(CONFIG).unwrap(),
        system: system().unwrap(),
    })
    .unwrap();
    let path = root
        .path()
        .join(native.as_str().trim_start_matches("sha256:"));
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read(root.path(), &native).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}
