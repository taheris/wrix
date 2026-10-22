use super::*;

fn execute_shell(script: &str, timeout: Duration) -> Result<Vec<u8>, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut command = controlled(Path::new("/bin/sh"));
    command.args(["-c", script]);
    runtime.block_on(run(command, &[], Instant::now() + timeout))
}

#[test]
fn exited_child_cleanup_preserves_successful_output() {
    let output = execute_shell("printf resolved", Duration::from_secs(5)).unwrap();
    assert_eq!(output, b"resolved");
}

#[test]
fn exited_child_cleanup_preserves_process_failure() {
    let error = execute_shell("exit 7", Duration::from_secs(5)).unwrap_err();
    assert!(matches!(error, Error::Invalid(message) if message == "resolver process failed"));
}

#[test]
fn running_child_cleanup_preserves_deadline_failure() {
    let error = execute_shell("while :; do :; done", Duration::from_millis(200)).unwrap_err();
    assert!(
        matches!(error, Error::Invalid(message) if message == "resolver execution deadline exceeded")
    );
}
