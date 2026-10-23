use super::{Operation, Reply};

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
