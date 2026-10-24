use super::{Error, Operation, Reply, acquire};
use std::time::Duration;
use tokio::{sync::Mutex, time::timeout};

#[tokio::test]
async fn maintenance_acquisition_is_immediate_when_native_work_holds_state() {
    let state = Mutex::new(());
    let _held = state.lock().await;
    for operation in [
        Operation::Inspect {},
        Operation::InspectImage {
            image: "a".repeat(64).try_into().unwrap(),
            paths: vec![
                "/nix/store/00000000000000000000000000000000-software"
                    .to_owned()
                    .try_into()
                    .unwrap(),
            ],
        },
        Operation::Collect {},
        Operation::Shutdown {},
    ] {
        let result = timeout(Duration::from_secs(1), acquire(&state, &operation))
            .await
            .unwrap();
        assert!(matches!(result, Err(Error::Busy)));
    }
}

#[tokio::test]
async fn trusted_seed_waits_for_existing_state_without_stealing_maintenance() {
    let state = Mutex::new(());
    let held = state.lock().await;
    let operation = Operation::Seed {
        image: "a".repeat(64).try_into().unwrap(),
        paths: vec![
            "/nix/store/00000000000000000000000000000000-software"
                .to_owned()
                .try_into()
                .unwrap(),
        ],
    };
    let queued = acquire(&state, &operation);
    tokio::pin!(queued);
    assert!(
        timeout(Duration::from_millis(20), &mut queued)
            .await
            .is_err()
    );
    drop(held);
    assert!(
        timeout(Duration::from_secs(1), &mut queued)
            .await
            .unwrap()
            .is_ok()
    );
}

#[test]
fn shutdown_cannot_select_a_process_program_or_filesystem_target() {
    assert!(matches!(
        serde_json::from_str::<Operation>(r#"{"operation":"shutdown"}"#).unwrap(),
        Operation::Shutdown {}
    ));
    for request in [
        r#"{"operation":"shutdown","pid":1}"#,
        r#"{"operation":"shutdown","program":"/bin/sh"}"#,
        r#"{"operation":"shutdown","path":"/nix"}"#,
        r#"{"operation":"shutdown","force":true}"#,
    ] {
        assert!(serde_json::from_str::<Operation>(request).is_err());
    }
}

#[test]
fn parameterless_maintenance_does_not_ignore_unknown_fields() {
    for operation in ["inspect", "collect", "shutdown"] {
        let request = format!(r#"{{"operation":"{operation}","path":"/outside-pool"}}"#);
        assert!(serde_json::from_str::<Operation>(&request).is_err());
    }
}

#[test]
fn shutdown_acceptance_does_not_serialize_as_completed_release() {
    assert_eq!(
        serde_json::to_string(&Reply::Stopping).unwrap(),
        r#"{"result":"stopping"}"#
    );
    let response: Reply = serde_json::from_str(r#"{"result":"stopping"}"#).unwrap();
    assert!(matches!(response, Reply::Stopping));
}
