use super::super::{Runtime, RuntimeKind};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};
use wrix_core::path::ContainerName;
use wrix_sandbox::image::Digest;

#[test]
fn apple_start_requires_stopped_bound_image_and_exact_ownership_before_execution() {
    let expected = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    for case in [
        "matching",
        "wrong-image",
        "wrong-boot",
        "already-running",
        "missing-image",
    ] {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("starts");
        let mut snapshot = json!([{
            "configuration": {
                "id": "wrix-candidate",
                "image": {"descriptor": {"digest": expected.as_str()}},
                "labels": {"wrix.kind":"service", "wrix.service.scope":"host", "wrix.service.boot":"candidate-boot"}
            },
            "status": {"state":"stopped", "networks":[]}
        }]);
        match case {
            "wrong-image" => {
                snapshot[0]["configuration"]["image"]["descriptor"]["digest"] =
                    json!(format!("sha256:{}", "b".repeat(64)));
            }
            "wrong-boot" => {
                snapshot[0]["configuration"]["labels"]["wrix.service.boot"] = json!("foreign-boot");
            }
            "already-running" => snapshot[0]["status"]["state"] = json!("running"),
            "missing-image" => snapshot[0]["configuration"]
                .as_object_mut()
                .unwrap()
                .remove("image")
                .map(|_| ())
                .unwrap(),
            _ => (),
        }
        let executable = root.path().join("container");
        let script = format!(
            "#!/bin/sh\nset -eu\ncase \"$1\" in\ninspect) printf '%s' '{}';;\nstart) printf 'start\\n' >>'{}';;\n*) exit 91;;\nesac\n",
            snapshot,
            log.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = Runtime {
            binary: executable.to_str().unwrap().to_owned(),
            kind: RuntimeKind::Container,
            binding: None,
        };
        let result = runtime.start_bound_apple(
            &ContainerName::from_persisted("wrix-candidate".to_owned()).unwrap(),
            &expected,
            &[
                ("wrix.kind", "service"),
                ("wrix.service.scope", "host"),
                ("wrix.service.boot", "candidate-boot"),
            ],
        );
        if case == "matching" {
            result.unwrap();
            assert_eq!(fs::read_to_string(&log).unwrap(), "start\n");
        } else {
            assert!(result.is_err(), "accepted {case}");
            assert!(!log.exists(), "started {case}");
        }
    }
}
