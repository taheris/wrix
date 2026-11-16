#![cfg(test)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(log: Option<&Path>, status: u8) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/lib/verifier-timing.sh");
    let mut command = Command::new("bash");
    command.args(["-c", "source \"$1\"; verifier_timed execute fixture bash -c 'printf output; printf diagnostic >&2; exit \"$1\"' ignored \"$2\"", "timing"])
        .arg(script).arg(status.to_string()).env_remove("WRIX_VERIFIER_TIMING_FILE");
    if let Some(log) = log {
        command.env("WRIX_VERIFIER_TIMING_FILE", log);
    }
    command.output().unwrap()
}

#[test]
fn optional_timing_preserves_output_and_all_exit_classes() {
    let temporary = tempfile::tempdir().unwrap();
    let log = temporary.path().join("timings.tsv");
    for status in [0, 1, 37, 77] {
        let normal = invoke(None, status);
        let timed = invoke(Some(&log), status);
        assert_eq!(normal.status.code(), Some(i32::from(status)));
        assert_eq!(normal.status, timed.status);
        assert_eq!(normal.stdout, timed.stdout);
        assert_eq!(normal.stderr, timed.stderr);
    }
    let data = fs::read_to_string(log).unwrap();
    let rows: Vec<_> = data.lines().collect();
    assert_eq!(rows.len(), 4);
    for (row, status) in rows.iter().zip([0, 1, 37, 77]) {
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(&fields[..2], &["execute", "fixture"]);
        assert!(fields[2].parse::<u64>().is_ok());
        assert_eq!(fields[3], status.to_string());
        assert!(fields[4].parse::<u32>().unwrap() > 0);
    }
}

#[test]
fn unavailable_diagnostics_never_turn_failure_into_success() {
    let temporary = tempfile::tempdir().unwrap();
    let output = invoke(Some(temporary.path()), 37);
    assert_eq!(output.status.code(), Some(37));
    assert_eq!(output.stdout, b"output");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("verifier timing output unavailable")
    );
}
