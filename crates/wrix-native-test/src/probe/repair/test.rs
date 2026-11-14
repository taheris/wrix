use super::*;
use std::{fs, os::unix::fs::PermissionsExt};
fn history() -> Vec<u8> {
    let mut bytes = b"commit_hash\n".to_vec();
    for n in 0..10 {
        bytes.extend(format!("{n:032x}\n").bytes());
    }
    bytes
}
fn plan() -> Plan {
    Plan::parse(b"issue_count\n7\n", b"issue_count\n9\n", &history()).unwrap()
}
#[test]
fn exact_issue_delta_and_ten_commits_form_a_read_only_query() {
    let query = plan().query();
    assert!(query.starts_with("SELECT COUNT(*) AS retained FROM dolt_log WHERE commit_hash IN ("));
    assert_eq!(query.matches("','").count(), 9);
    assert!(!query.contains(';'));
}
#[test]
fn changed_issue_counts_or_unsigned_overflow_cannot_form_a_plan() {
    for (before, after) in [
        ("7", "7"),
        ("7", "8"),
        ("7", "10"),
        ("18446744073709551615", "1"),
        ("-1", "1"),
    ] {
        assert!(
            Plan::parse(
                format!("issue_count\n{before}\n").as_bytes(),
                format!("issue_count\n{after}\n").as_bytes(),
                &history()
            )
            .is_err()
        );
    }
}
#[test]
fn count_requires_one_unambiguous_record() {
    for data in [
        b"issue_count\n1\n2\n".as_slice(),
        b"issue_count\n\n",
        b"other\n7\n",
    ] {
        assert!(Plan::parse(data, b"issue_count\n9\n", &history()).is_err());
    }
}
#[test]
fn hash_parser_preserves_all_original_supported_formats_without_sql_injection() {
    for data in ["z".repeat(32), "f".repeat(40), "a".repeat(64)] {
        assert!(Commit::parse(&data).is_ok());
    }
    for data in [
        "a".repeat(31),
        "G".repeat(40),
        "z".repeat(40),
        "x'; DELETE FROM dolt_log; --".into(),
        "A".repeat(32),
    ] {
        assert!(Commit::parse(&data).is_err());
    }
}
#[test]
fn missing_or_extra_history_rows_are_refused() {
    let data = history();
    let one = b"commit_hash\n00000000000000000000000000000000\n";
    assert!(Plan::parse(b"issue_count\n7\n", b"issue_count\n9\n", one).is_err());
    let mut extra = data;
    extra.extend(b"00000000000000000000000000000000\n");
    assert!(Plan::parse(b"issue_count\n7\n", b"issue_count\n9\n", &extra).is_err());
}
#[test]
fn native_resource_parser_refuses_ambiguous_or_changed_backend_selection() {
    for value in [
        serde_json::json!([]),
        serde_json::json!([{}, {}]),
        serde_json::json!([{"configuration":{"resources":{"cpus":3,"memoryInBytes":1_073_741_824}}}]),
        serde_json::json!([{"configuration":{"resources":{"cpus":4,"memoryInBytes":1}}}]),
        serde_json::json!([{"configuration":{"resources":{"cpus":"4","memoryInBytes":1_073_741_824}}}]),
    ] {
        assert!(Resources::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
fn executable(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::write(
        &path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn actual_read_only_process_boundary_retains_only_resource_fields() {
    let root = tempfile::tempdir().unwrap();
    let bd = executable(
        root.path(),
        "bd",
        "[[ $# == 3 && $1 == sql && $2 == --csv && $3 == SELECT* ]]\nprintf 'retained\\n10\\n'",
    );
    let container = executable(
        root.path(),
        "container",
        "[[ $# == 2 && $1 == inspect && $2 == wrix-host-service ]]\nprintf '%s\\n' '[{\"configuration\":{\"environment\":{\"token\":\"not-a-real-secret\"},\"resources\":{\"cpus\":4,\"memoryInBytes\":1073741824,\"other\":true}}}]'",
    );
    let receipt = plan().run(&bd, &container).unwrap();
    let value = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        value["issue_count_preserved_with_two_explicit_new_tasks"],
        true
    );
    assert_eq!(value["ten_premaintenance_commits_still_reachable"], true);
    assert_eq!(value["retained_backend_resources"]["other"], true);
    assert!(!value.to_string().contains("token"));
}
#[test]
fn fewer_retained_commits_prevent_container_inspection() {
    let root = tempfile::tempdir().unwrap();
    let bd = executable(root.path(), "bd", "printf 'retained\\n9\\n'");
    assert!(matches!(
        plan().run(&bd, &root.path().join("does-not-exist")),
        Err(Error::Retained)
    ));
}
#[test]
fn failing_database_command_cannot_produce_a_receipt() {
    let root = tempfile::tempdir().unwrap();
    let bd = executable(root.path(), "bd", "exit 7");
    assert!(matches!(
        plan().run(&bd, &root.path().join("does-not-exist")),
        Err(Error::Failed)
    ));
}
