use super::*;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use wrix_core::path::Workspace;

#[test]
fn shell_handoff_preserves_exact_recipes_without_executing_them_early() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_path(root.path()).unwrap();
    let shell = Command::new("sh")
        .args(["-c", "command -v sh"])
        .output()
        .unwrap();
    assert!(shell.status.success());
    let shell = String::from_utf8(shell.stdout).unwrap();
    let executable = root.path().join("wrix-service-supervisor");
    fs::write(
        &executable,
        format!("#!{}\nprintf '%s\\0' \"$@\"\n", shell.trim()),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let marker = root.path().join("must-not-run");
    let first = "exec celler --value 'a b' \"$UNCHANGED\"";
    let second = format!(
        "dolt sql -q \"CREATE USER 'root'@'%'; $(touch '{}')\"",
        marker.display()
    );
    let output = Command::new(shell.trim())
        .arg("-c")
        .arg(command(workspace.hash(), first, &second))
        .env("PATH", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let words: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|word| !word.is_empty())
        .collect();
    assert_eq!(
        words,
        [
            b"serve".as_slice(),
            DIRECTORY.as_bytes(),
            workspace.hash().as_str().as_bytes(),
            b"/run/wrix/dolt.sock",
            first.as_bytes(),
            second.as_bytes()
        ]
    );
    assert!(!marker.exists());
}
