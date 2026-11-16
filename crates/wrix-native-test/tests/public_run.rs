use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
const TOOL: &str = env!("CARGO_BIN_EXE_wrix-public-run");
fn execute(program: &Path, args: &[&std::ffi::OsStr]) -> std::io::Result<Output> {
    Command::new(program).args(args).output()
}
#[test]
fn public_run_requires_an_explicit_executable_and_existing_profile() {
    let output = Command::new(TOOL)
        .args(["--cli", "relative", "/absent"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
}
#[test]
fn a_failed_diagnostic_write_returns_a_distinct_io_failure_status() {
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    let descriptor: std::os::fd::OwnedFd = writer.into();
    let output = Command::new(TOOL)
        .args(["--cli", "relative", "/absent"])
        .stderr(std::process::Stdio::from(descriptor))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
}
#[test]
fn real_pty_preserves_fixed_public_command_arguments_terminal_output_and_status() {
    let files = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(files.path()).unwrap();
    let profile = root.join("profile with spaces.json");
    fs::write(&profile, b"{}\n").unwrap();
    let cli = root.join("fixed-cli");
    fs::write(&cli, b"#!/usr/bin/env bash\nset -euo pipefail\ntest -t 0; test -t 1; test -t 2\nprintf 'controlling-terminal\\n' > /dev/tty\nprintf '<%s>\\n' \"$@\"\nprintf 'diagnostic\\n' >&2\nexit 37\n").unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    let output = execute(
        Path::new(TOOL),
        &["--cli".as_ref(), cli.as_os_str(), profile.as_os_str()],
    )
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
    let expected = format!("controlling-terminal\r\n<--profile-config>\r\n<{}>\r\n<run>\r\n<--nix-store>\r\n<shared>\r\n<--no-cache>\r\n<-->\r\n<public-second>\r\n<diagnostic>\r\n", profile.display()).replace("<diagnostic>", "diagnostic");
    assert_eq!(output.stdout, expected.as_bytes());
}
