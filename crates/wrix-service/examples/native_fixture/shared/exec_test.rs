use super::*;
use std::{io::Write, os::unix::fs::PermissionsExt};

const ROOT: &str = "WRIX_NATIVE_EXEC_STDIN_TEST_ROOT";
const REPLY: &str = "{\"rows\":[{\"fixture\":true}]}";
const DIAGNOSTIC: &str = "native-exec-stderr";

fn check_stdin(interactive: bool, test: &str) {
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let mut args = vec![OsString::from("podman"), OsString::from("exec")];
        if interactive {
            args.push(OsString::from("-i"));
        }
        args.extend([
            OsString::from("owned"),
            root.join("backend").into_os_string(),
        ]);
        assert_eq!(run(&args).unwrap(), ExitCode::from(17));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let nsenter = bin.join("nsenter");
    fs::write(
        &nsenter,
        "#!/usr/bin/env bash\nset -euo pipefail\n[[ $# == 5 && $1 == --mount && $2 == --target && $3 == 42 && $4 == -- ]]\nshift 4\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&nsenter, fs::Permissions::from_mode(0o700)).unwrap();
    let backend = root.path().join("backend");
    fs::write(
        &backend,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ncat > stdin\nprintf '%s\\n' '{REPLY}'\nprintf '%s\\n' '{DIAGNOSTIC}' >&2\nexit 17\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
    let state: State = serde_json::from_value(json!({
        "name": "owned", "running": true, "labels": {}, "ports": [],
        "pid": 42, "data": root.path()
    }))
    .unwrap();
    save(&root.path().join("runtime.json"), &state).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let payload = if interactive {
        b"SELECT 1;\nSELECT 'unchanged bytes';\n".repeat(1024)
    } else {
        b"not forwarded".to_vec()
    };
    let input = root.path().join("input");
    fs::write(&input, &payload).unwrap();
    let stdin = if interactive {
        Stdio::piped()
    } else {
        Stdio::from(fs::File::open(input).unwrap())
    };
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(ROOT, root.path())
        .env("WRIX_SHARED_FIXTURE", root.path())
        .env("PATH", path)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if interactive {
        child.stdin.take().unwrap().write_all(&payload).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(root.path().join("stdin")).unwrap(),
        if interactive { payload } else { Vec::new() }
    );
    assert!(String::from_utf8(output.stdout).unwrap().contains(REPLY));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains(DIAGNOSTIC)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("native-exec.stderr")).unwrap(),
        format!("{DIAGNOSTIC}\n")
    );
}

#[test]
fn interactive_exec_forwards_stdin() {
    check_stdin(
        true,
        "native_fixture::shared::exec_test::interactive_exec_forwards_stdin",
    );
}

#[test]
fn noninteractive_exec_does_not_forward_stdin() {
    check_stdin(
        false,
        "native_fixture::shared::exec_test::noninteractive_exec_does_not_forward_stdin",
    );
}
