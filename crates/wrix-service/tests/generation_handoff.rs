use std::{ffi::OsString, fs, path::Path};

use serde_json::{Value, json};
use wrix_service::lifecycle::binding::{Candidate, Error};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn binding() -> Value {
    json!({
        "schema": 1,
        "image": "localhost/wrix-service:immutable",
        "source_kind": "nix-descriptor",
        "source": {"path": "/nix/store/source", "derivation": "/nix/store/source.drv"},
        "digest": {"path": "/nix/store/layout/config-digest", "derivation": "/nix/store/layout.drv"}
    })
}

fn write(path: &Path, value: &Value) -> Result<(), std::io::Error> {
    fs::write(path, serde_json::to_vec(value)?)
}

#[test]
fn update_target_uses_explicit_package_image_binding() -> TestResult {
    let root = tempfile::tempdir()?;
    let package_a = root.path().join("host-a.json");
    let package_b = root.path().join("different-installation.json");
    write(&package_a, &binding())?;
    write(&package_b, &binding())?;
    let selected = Candidate::resolve(Some(&package_a), |_| None)?;
    let equivalent = Candidate::resolve(Some(&package_b), |_| None)?;
    assert_eq!(selected, equivalent);
    assert_eq!(
        selected.image().as_str(),
        "localhost/wrix-service:immutable"
    );
    assert_eq!(selected.source().path(), Path::new("/nix/store/source"));
    assert!(matches!(
        Candidate::resolve(None, |_| Some(OsString::from("project-substitute"))),
        Err(Error::Missing)
    ));
    for name in [
        "WRIX_SERVICE_IMAGE",
        "WRIX_SERVICE_IMAGE_SOURCE",
        "WRIX_SERVICE_IMAGE_SOURCE_KIND",
        "WRIX_SERVICE_IMAGE_DIGEST",
    ] {
        assert!(
            matches!(Candidate::resolve(Some(&package_a), |key| (key == name).then(|| OsString::from("project-substitute"))), Err(Error::Conflict(key)) if key == name)
        );
    }
    assert_eq!(
        selected,
        Candidate::resolve(Some(&package_a), |key| {
            matches!(key, "PWD" | "WRIX_PROFILE_CONFIG" | "WRIX_SERVICE_VERSION")
                .then(|| OsString::from("unrelated"))
        })?
    );
    let mut other = binding();
    other["image"] = json!("localhost/wrix-service:other");
    write(&package_b, &other)?;
    assert_ne!(selected, Candidate::resolve(Some(&package_b), |_| None)?);
    Ok(())
}

#[test]
fn binding_rejects_missing_malformed_and_unknown_metadata() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("binding.json");
    for field in ["schema", "image", "source_kind", "source", "digest"] {
        let mut value = binding();
        value.as_object_mut().unwrap().remove(field);
        write(&path, &value)?;
        assert!(Candidate::load(&path).is_err(), "{field}");
    }
    for (field, value) in [
        ("schema", json!(2)),
        ("image", json!("bad image")),
        ("source_kind", json!("tarball")),
        ("extra", json!(true)),
    ] {
        let mut wire = binding();
        wire[field] = value;
        write(&path, &wire)?;
        assert!(Candidate::load(&path).is_err(), "{field}");
    }
    for artifact in ["source", "digest"] {
        for (field, invalid) in [
            ("path", "relative"),
            ("path", "/tmp/../escape"),
            ("path", "/tmp/bad\0path"),
            ("derivation", "/tmp/not-a-derivation"),
        ] {
            let mut wire = binding();
            wire[artifact][field] = json!(invalid);
            write(&path, &wire)?;
            assert!(Candidate::load(&path).is_err(), "{artifact}/{field}");
        }
    }
    Ok(())
}

#[test]
fn binding_accepts_identical_ambient_metadata_without_using_it_as_authority() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("binding.json");
    write(&path, &binding())?;
    let candidate = Candidate::resolve(Some(&path), |key| match key {
        "WRIX_SERVICE_IMAGE" => Some("localhost/wrix-service:immutable".into()),
        "WRIX_SERVICE_IMAGE_SOURCE" => Some("/nix/store/source".into()),
        "WRIX_SERVICE_IMAGE_SOURCE_KIND" => Some("nix-descriptor".into()),
        "WRIX_SERVICE_IMAGE_DIGEST" => Some("/nix/store/layout/config-digest".into()),
        _ => None,
    })?;
    assert_eq!(candidate, Candidate::load(&path)?);
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[test]
fn bound_artifact_realization_checks_native_outputs_before_building() -> TestResult {
    use std::{env, os::unix::fs::PermissionsExt, process::Command};
    const CHILD: &str = "WRIX_TEST_BINDING_REALIZATION";
    if let Ok(mode) = env::var(CHILD) {
        let candidate = Candidate::from_env()?;
        let roots = env::var_os("WRIX_TEST_ROOTS").unwrap();
        let result = candidate.realize(Path::new(&roots));
        match mode.as_str() {
            "success" => assert_eq!(result?.as_str(), format!("sha256:{}", "a".repeat(64))),
            "mismatch" => assert!(matches!(result, Err(Error::Metadata(_)))),
            "query-failure" | "build-failure" => {
                assert!(matches!(result, Err(Error::Derivation(_))));
            }
            "digest" => assert!(matches!(result, Err(Error::Digest(_)))),
            _ => panic!("unknown test mode"),
        }
        return Ok(());
    }
    for mode in [
        "success",
        "mismatch",
        "query-failure",
        "build-failure",
        "digest",
    ] {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        let source = root.path().join("source");
        let digest = root.path().join("digest");
        let manifest = root.path().join("binding.json");
        fs::write(&source, "selected image")?;
        fs::write(
            &digest,
            if mode == "digest" {
                "bad".to_owned()
            } else {
                format!("sha256:{}", "a".repeat(64))
            },
        )?;
        let mut wire = binding();
        for (field, path) in [("source", &source), ("digest", &digest)] {
            let drv = path.with_extension("drv");
            fs::write(&drv, path.as_os_str().as_encoded_bytes())?;
            wire[field] = json!({"path": path, "derivation": drv});
        }
        write(&manifest, &wire)?;
        let nix_store = bin.join("nix-store");
        fs::write(
            &nix_store,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$WRIX_TEST_CALLS"
case "$1" in
  --query)
    [[ "$2" == --outputs ]]
    if [[ "$WRIX_TEST_BINDING_REALIZATION" == query-failure ]]; then exit 1; fi
    if [[ "$WRIX_TEST_BINDING_REALIZATION" == mismatch ]]; then printf '/wrong/output\n'; else cat "$3"; printf '\n'; fi
    ;;
  --realise)
    [[ "$3" == --add-root && "$5" == --indirect ]]
    if [[ "$WRIX_TEST_BINDING_REALIZATION" == build-failure ]]; then exit 1; fi
    ln -s "$(cat "$2")" "$4"
    ;;
  *) exit 64 ;;
esac
"#,
        )?;
        fs::set_permissions(&nix_store, fs::Permissions::from_mode(0o755))?;
        let roots = root.path().join("roots");
        let calls = root.path().join("calls");
        let path = env::join_paths(
            std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )?;
        let output = Command::new(env::current_exe()?)
            .args([
                "--exact",
                "bound_artifact_realization_checks_native_outputs_before_building",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env("PATH", path)
            .env("WRIX_SERVICE_PACKAGE_BINDING", &manifest)
            .env("WRIX_TEST_ROOTS", &roots)
            .env("WRIX_TEST_CALLS", &calls)
            .env_remove("WRIX_SERVICE_IMAGE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST")
            .output()?;
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = fs::read_to_string(&calls)?;
        if matches!(mode, "mismatch" | "query-failure") {
            assert!(!calls.contains("--realise"));
            assert!(!roots.exists());
        } else if mode == "success" {
            assert_eq!(fs::read_link(roots.join("source"))?, source);
            assert_eq!(fs::read_link(roots.join("digest"))?, digest);
            assert_eq!(calls.lines().count(), 4);
            assert!(
                calls
                    .lines()
                    .take(2)
                    .all(|line| line.starts_with("--query --outputs"))
            );
        }
    }
    Ok(())
}
