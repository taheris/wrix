//! Assertions consume actual guest reports and native wire output, never source text or invented runtime inspection.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing measured field {key}: {value}"))
}
pub(super) fn independent(platform: Platform, first: &Value, second: &Value) {
    assert_eq!(string(first, "system"), system().as_str());
    assert_eq!(first["system"], second["system"]);
    assert_eq!(first["path"], second["path"]);
    assert_eq!(first["prior_miss"], true);
    assert_eq!(second["prior_miss"], true);
    if matches!(platform, Platform::Apple | Platform::Krun) {
        assert_ne!(
            string(first, "boot_id"),
            string(second, "boot_id"),
            "workers share a kernel instead of independent VMs"
        );
    }
    assert_ne!(string(first, "machine"), "");
}
pub(super) fn service_boundary(platform: Platform, runtime: &Command, owner: &str, worker: &Value) {
    let output = worker::execute(
        runtime,
        &["exec", owner, "/bin/cat", "/proc/sys/kernel/random/boot_id"],
    );
    let boot = String::from_utf8(output.stdout).unwrap();
    assert_ne!(
        boot.trim(),
        string(worker, "boot_id"),
        "pool service and worker share one VM"
    );
    if platform == Platform::Apple {
        let output = worker::execute(runtime, &["inspect", owner]);
        let values: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(values.len(), 1);
        let config = &values[0]["configuration"];
        assert_eq!(values[0]["id"], owner);
        assert!(
            config["creationDate"].as_str().is_some(),
            "native Apple ISO8601 date required, not Podman timestamps"
        );
        assert_eq!(config["runtimeHandler"], "container-runtime-linux");
        assert_ne!(config["image"]["descriptor"]["digest"], Value::Null);
        assert_eq!(values[0]["status"]["state"], "running");
    }
}
pub(super) fn workflows(first: &Value, next: &Value) {
    assert_eq!(string(first, "path"), string(next, "path"));
    assert_eq!(string(first, "run_stdout"), "installed\n");
    assert_eq!(string(next, "run_stdout"), "installed\n");
    for field in [
        "build",
        "develop",
        "run",
        "profile",
        "cargo",
        "clippy",
        "nextest",
        "check",
        "readonly",
        "untrusted",
        "strict",
        "signature_denied",
        "repair_denied",
        "options_denied",
        "admin_denied",
        "outside_denied",
        "peer_denied",
        "dynamic_visible",
    ] {
        assert_eq!(first[field], true, "first workflow did not verify {field}");
        assert_eq!(next[field], true, "fresh worker did not verify {field}");
    }
    assert!(first["elapsed_ns"].as_u64().unwrap() > 0);
    assert!(next["elapsed_ns"].as_u64().unwrap() > 0);
    for value in [first, next] {
        assert!(value["io"]["rchar"].is_number());
        assert!(value["io"]["wchar"].is_number());
        assert!(value["io"]["read_bytes"].is_number());
        assert!(value["io"]["write_bytes"].is_number());
    }
}
#[derive(Default, Debug, Serialize, Eq, PartialEq)]
struct Activities {
    copy_path: u64,
    file_transfer: u64,
    build: u64,
    transferred_progress_bytes: u64,
    upstream_nar_requests: u64,
    upstream_nar_bytes: u64,
    selected_derivation_builds: u64,
    native_substitutions: u64,
}
#[derive(Deserialize)]
struct Event {
    action: String,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default, rename = "type")]
    kind: Option<u64>,
    #[serde(default)]
    fields: Vec<Value>,
}
fn activities(bytes: &[u8]) -> Activities {
    let text = std::str::from_utf8(bytes).unwrap();
    let mut measured = Activities::default();
    let mut transfers = BTreeMap::<u64, (bool, u64)>::new();
    for line in text.lines() {
        let Some(event) = line.strip_prefix("@nix ") else {
            continue;
        };
        let event: Event = serde_json::from_str(event).expect("actual Nix internal-json event");
        if event.action == "start" {
            match event.kind {
                Some(100) => measured.copy_path += 1,
                Some(101) => {
                    measured.file_transfer += 1;
                    let nar = event
                        .fields
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|field| field.starts_with("https://cache.nixos.org/nar/"));
                    if nar {
                        measured.upstream_nar_requests += 1;
                    }
                    transfers.insert(event.id.unwrap(), (nar, 0));
                }
                Some(108) => measured.native_substitutions += 1,
                Some(105) => {
                    measured.build += 1;
                    if event
                        .fields
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|field| field.ends_with("-wrix-native-installed-platform.drv"))
                    {
                        measured.selected_derivation_builds += 1;
                    }
                }
                _ => (),
            }
        } else if event.action == "result"
            && event.kind == Some(105)
            && let Some((_, bytes)) = transfers.get_mut(&event.id.unwrap())
        {
            *bytes = (*bytes).max(
                event
                    .fields
                    .first()
                    .and_then(Value::as_u64)
                    .expect("Nix progress bytes"),
            );
        }
    }
    for (nar, bytes) in transfers.into_values() {
        measured.transferred_progress_bytes = measured
            .transferred_progress_bytes
            .checked_add(bytes)
            .unwrap();
        if nar {
            measured.upstream_nar_bytes = measured.upstream_nar_bytes.checked_add(bytes).unwrap();
        }
    }
    measured
}
pub(super) fn report(platform: Platform, control: &Path, workers: &[Value], elapsed: Duration) {
    let mut observations = Vec::new();
    let mut builds = 0;
    let mut nar_requests = 0;
    let mut nar_bytes = 0;
    let mut imports = 0;
    for (name, worker) in ["first", "second", "replacement"].into_iter().zip(workers) {
        let log = fs::read(control.join(format!("{name}-nix-events.log"))).unwrap();
        let substitution =
            activities(&fs::read(control.join(format!("{name}-substitution-events.log"))).unwrap());
        let build =
            activities(&fs::read(control.join(format!("{name}-build-events.log"))).unwrap());
        builds += build.selected_derivation_builds;
        nar_requests += substitution.upstream_nar_requests;
        nar_bytes += substitution.upstream_nar_bytes;
        imports += substitution.native_substitutions;
        if name == "replacement" {
            assert_eq!(
                substitution.upstream_nar_requests, 0,
                "fresh worker repeated upstream NAR retrieval"
            );
            assert_eq!(
                substitution.upstream_nar_bytes, 0,
                "fresh worker repeated upstream NAR traffic"
            );
            assert_eq!(
                substitution.native_substitutions, 0,
                "fresh worker repeated native substitution/import"
            );
            assert_eq!(
                substitution.copy_path, 0,
                "fresh worker repeated native path import"
            );
            assert_eq!(
                build.selected_derivation_builds, 0,
                "fresh worker rebuilt the installed derivation"
            );
        }
        observations.push(json!({"worker": name, "guest": worker, "workflow_activities": activities(&log), "substitution": substitution, "realization": build}));
    }
    assert!(
        nar_requests > 0 && nar_bytes > 0,
        "UNAVAILABLE: actual native daemon logs did not expose cold upstream NAR requests/bytes; zero is not evidence"
    );
    assert!(
        imports > 0,
        "UNAVAILABLE: native substitution/import activity was not observed during the known cold transfer"
    );
    assert_eq!(
        builds, 1,
        "identical concurrent derivation was not observed building exactly once"
    );
    let resource_results = read(&control.join("first-resources.json"));
    for field in [
        "public_fod",
        "private_fod_denied",
        "ipv6_fod_denied",
        "ordinary_network_denied",
        "disk_bound",
        "pids_bound",
        "cpu_work_completed",
        "oom_failed",
        "live_output_preserved",
    ] {
        assert_eq!(
            resource_results[field], true,
            "missing actual resource result {field}"
        );
    }
    let report = json!({
        "service_before": read(&control.join("service-before.json")),
        "service_after": read(&control.join("service-after.json")),
        "resource_results": resource_results,
        "platform": format!("{platform:?}"),
        "boundary": "isolated production factory; public Run/Spawn remains gated",
        "elapsed_ns": u64::try_from(elapsed.as_nanos()).unwrap(),
        "workers": observations,
        "counter_scope": "actual Nix internal-json activity starts and final transfer-progress high-water marks; cold transfer and one selected build are mandatory positive controls"
    });
    fs::write(
        control
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("native-measurements.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn actual_nix_events_measure_transfers_and_builds_without_confusing_messages_with_events() {
    let events = b"ordinary diagnostic\n@nix {\"action\":\"start\",\"id\":7,\"type\":101,\"fields\":[]}\n@nix {\"action\":\"result\",\"id\":7,\"type\":105,\"fields\":[37,90,0,0]}\n@nix {\"action\":\"start\",\"id\":8,\"type\":100}\n@nix {\"action\":\"start\",\"id\":9,\"type\":105}\n";
    assert_eq!(
        activities(events),
        Activities {
            copy_path: 1,
            file_transfer: 1,
            build: 1,
            transferred_progress_bytes: 37,
            upstream_nar_requests: 0,
            upstream_nar_bytes: 0,
            selected_derivation_builds: 0,
            native_substitutions: 0
        }
    );
    assert!(std::panic::catch_unwind(|| activities(b"@nix not-json")).is_err());
}
#[test]
fn native_transfer_progress_uses_high_water_bytes_and_counts_only_actual_nar_requests() {
    let events = b"@nix {\"action\":\"start\",\"id\":7,\"type\":101,\"fields\":[\"https://cache.nixos.org/nar/a.nar.xz\"]}\n@nix {\"action\":\"result\",\"id\":7,\"type\":105,\"fields\":[37,90,0,0]}\n@nix {\"action\":\"result\",\"id\":7,\"type\":105,\"fields\":[90,90,0,0]}\n@nix {\"action\":\"start\",\"id\":8,\"type\":101,\"fields\":[\"https://cache.nixos.org/test.narinfo\"]}\n@nix {\"action\":\"start\",\"id\":9,\"type\":105,\"fields\":[\"/nix/store/00000000000000000000000000000000-wrix-native-installed-platform.drv\"]}\n";
    let mut events = events.to_vec();
    events.extend_from_slice(b"@nix {\"action\":\"start\",\"id\":10,\"type\":108,\"fields\":[\"/nix/store/00000000000000000000000000000000-cold-dependency\"]}\n");
    let measured = activities(&events);
    assert_eq!(measured.native_substitutions, 1);
    assert_eq!(measured.upstream_nar_requests, 1);
    assert_eq!(measured.upstream_nar_bytes, 90);
    assert_eq!(measured.transferred_progress_bytes, 90);
    assert_eq!(measured.selected_derivation_builds, 1);
}
#[test]
fn native_vm_boundary_rejects_reused_kernels_and_missing_prior_negative_lookup() {
    let ready = json!({"system": system().as_str(), "path": "/nix/store/00000000000000000000000000000000-dynamic", "prior_miss": true, "boot_id": "same-kernel", "machine": "native"});
    assert!(std::panic::catch_unwind(|| independent(Platform::Apple, &ready, &ready)).is_err());
    let mut different = ready.clone();
    different["boot_id"] = json!("independent-kernel");
    independent(Platform::Krun, &ready, &different);
    different["prior_miss"] = json!(false);
    assert!(std::panic::catch_unwind(|| independent(Platform::Krun, &ready, &different)).is_err());
}
