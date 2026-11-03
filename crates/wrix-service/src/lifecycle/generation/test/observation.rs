use super::*;

#[test]
fn native_identity_and_ports_share_one_current_inspection() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("container");
    let snapshot = root.path().join("snapshot.json");
    let calls = root.path().join("calls");
    std::fs::write(
        &binary,
        "#!/bin/bash\nset -euo pipefail\n[[ $# == 2 && $1 == inspect && $2 == wrix-host-service ]] || exit 64\nroot=\"${0%/*}\"\nprintf '%s\\n' \"$*\" >>\"$root/calls\"\ncat \"$root/snapshot.json\"\n",
    ).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = Runtime {
        binary: binary.to_str().unwrap().to_owned(),
        kind: RuntimeKind::Container,
        binding: None,
    };
    let plan = plan();
    for case in ["valid", "stopped", "image", "kind", "scope", "missing"] {
        let mut inventory = serde_json::json!([{
            "configuration": {
                "id": "wrix-host-service",
                "image": {"descriptor": {"digest": plan.backend.recipe.image_id.as_str()}},
                "labels": {"wrix.kind": "service", "wrix.service.scope": "host"},
                "publishedPorts": [{"hostPort": 31003, "count": 1}]
            },
            "status": {"state": "running"}
        }]);
        match case {
            "stopped" => inventory[0]["status"]["state"] = serde_json::json!("stopped"),
            "image" => {
                inventory[0]["configuration"]["image"]["descriptor"]["digest"] =
                    serde_json::json!(format!("sha256:{}", "c".repeat(64)));
            }
            "kind" => {
                inventory[0]["configuration"]["labels"]["wrix.kind"] = serde_json::json!("worker");
            }
            "scope" => {
                inventory[0]["configuration"]["labels"]["wrix.service.scope"] =
                    serde_json::json!("workspace");
            }
            "missing" => inventory = serde_json::json!([]),
            "valid" => (),
            _ => panic!("unknown observation case"),
        }
        std::fs::write(&snapshot, serde_json::to_vec(&inventory).unwrap()).unwrap();
        std::fs::write(&calls, "").unwrap();
        let observed = verify(&runtime, &plan.backend.name.0, &plan.backend.recipe);
        if case == "valid" {
            let Verified::Apple(observed) = observed.unwrap() else {
                panic!("Apple verification discarded the native observation");
            };
            assert_eq!(observed.into_info().published_ports, [31003]);
        } else {
            assert!(observed.is_err(), "accepted changed {case}");
        }
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "inspect wrix-host-service\n"
        );
    }
}
