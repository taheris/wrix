use super::super::test::{key, target};
use super::*;
use serde_json::json;
use std::{collections::BTreeSet, os::unix::fs::PermissionsExt};
use wrix_state::execution::Ticket;

fn fixture() -> (tempfile::TempDir, deployment::Id, Key) {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let deployment = serde_json::from_value(json!("d".repeat(32))).unwrap();
    let pool = Key {
        repository: "e".repeat(32).try_into().unwrap(),
        system: wrix_core::nix_store::pool::System::X86_64Linux,
    };
    (root, deployment, pool)
}

fn publish(root: &Path, deployment: &deployment::Id, pool: &Key, state: serde_json::Value) {
    let record = Record {
        version: 1,
        deployment: deployment.clone(),
        pool: pool.clone(),
        state: serde_json::from_value(state).unwrap(),
        executions: BTreeSet::new(),
    };
    record.publish(root).unwrap();
}

#[test]
fn every_recorded_phase_reports_immutable_software_without_probing_or_mutating() {
    let (root, deployment, pool) = fixture();
    let connection = json!({"target":target(),"port":2222,"host_key":key()});
    for (phase, state) in [
        (
            "starting",
            json!({"phase":"starting","attempt":target()["attempt"]}),
        ),
        ("created", json!({"phase":"created","target":target()})),
        (
            "serving",
            json!({"phase":"serving","connection":connection}),
        ),
        (
            "stopping",
            json!({"phase":"stopping","connection":connection}),
        ),
        (
            "released",
            json!({"phase":"released","connection":connection}),
        ),
    ] {
        publish(root.path(), &deployment, &pool, state);
        let before = fs::read(root.path().join(LEDGER)).unwrap();
        let snapshot = Snapshot::read(root.path(), &deployment, &pool).unwrap();
        assert_eq!(snapshot.phase.as_str(), phase);
        assert_eq!(
            snapshot.image.as_str(),
            format!("sha256:{}", "c".repeat(64))
        );
        assert_eq!(snapshot.retained, 0);
        let mut output = Vec::new();
        snapshot.write(pool.system, &mut output).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "x86_64-linux: recorded native owner {phase}; readiness not probed\nx86_64-linux: recorded execution protection: 0 retained; live readers/work not probed\nx86_64-linux: recorded owner image sha256:{}; compatibility not probed\n",
                "c".repeat(64)
            )
        );
        assert_eq!(fs::read(root.path().join(LEDGER)).unwrap(), before);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn missing_execution_records_cannot_report_zero_retained_protection() {
    let (root, deployment, pool) = fixture();
    publish(
        root.path(),
        &deployment,
        &pool,
        json!({"phase":"created","target":target()}),
    );
    let mut record = Record::read(root.path(), &deployment, &pool).unwrap();
    record
        .executions
        .insert(Ticket::parse(&"a".repeat(64)).unwrap());
    record.publish(root.path()).unwrap();
    assert!(Snapshot::read(root.path(), &deployment, &pool).is_err());
    assert!(!root.path().join("executions").exists());
}

#[test]
fn passive_observation_refuses_foreign_deployment_and_pool_records() {
    let (root, deployment, pool) = fixture();
    publish(
        root.path(),
        &deployment,
        &pool,
        json!({"phase":"created","target":target()}),
    );
    let other = serde_json::from_value(json!("f".repeat(32))).unwrap();
    assert!(Snapshot::read(root.path(), &other, &pool).is_err());
    let peer = Key {
        repository: "f".repeat(32).try_into().unwrap(),
        system: pool.system,
    };
    assert!(Snapshot::read(root.path(), &deployment, &peer).is_err());
}
