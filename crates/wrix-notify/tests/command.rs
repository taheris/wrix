use std::{
    ffi::OsString,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use wrix_notify::{
    command::{self, Mode},
    process::{self, Error},
    route::Platform,
};

#[test]
fn modes_are_mutually_exclusive_and_unknown_arguments_fail() {
    for args in [
        vec!["focus", "--enqueue", "a", "--return"],
        vec!["focus", "--is-focused", "a", "--enqueue", "b"],
        vec!["focus", "--return", "--is-focused", "a"],
        vec!["focus", "--unknown"],
        vec!["focus", "--enqueue"],
    ] {
        assert!(command::parser().try_get_matches_from(args).is_err());
    }
}

#[test]
fn default_and_explicit_modes_parse_without_changing_opaque_targets() {
    let matches = command::parser().try_get_matches_from(["focus"]).unwrap();
    assert!(matches!(command::mode(&matches).unwrap(), Mode::Focus));
    let target = " host/é:\"quoted\"\n";
    let matches = command::parser()
        .try_get_matches_from(["focus", "--enqueue", target])
        .unwrap();
    let Mode::Enqueue(parsed) = command::mode(&matches).unwrap() else {
        panic!("wrong mode");
    };
    assert_eq!(parsed.as_str(), target);
    let matches = command::parser()
        .try_get_matches_from(["focus", "--return"])
        .unwrap();
    assert!(matches!(command::mode(&matches).unwrap(), Mode::Return));
}

#[test]
fn gui_and_shell_use_the_same_darwin_session_directory() {
    for xdg in [None, Some("/custom/data"), Some("")] {
        assert_eq!(
            command::session_directory(Platform::Darwin, |key| match key {
                "HOME" => Some(OsString::from("/Users/user")),
                "XDG_DATA_HOME" => xdg.map(OsString::from),
                _ => None,
            })
            .unwrap(),
            PathBuf::from("/Users/user/.local/share/wrix/sessions")
        );
    }
    assert!(command::session_directory(Platform::Darwin, |_| None).is_err());
}

#[test]
fn linux_session_directory_preserves_runtime_and_home_fallback() {
    assert_eq!(
        command::session_directory(Platform::Linux, |key| (key == "XDG_RUNTIME_DIR")
            .then(|| OsString::from("/runtime")))
        .unwrap(),
        PathBuf::from("/runtime/wrix/sessions")
    );
    assert_eq!(
        command::session_directory(Platform::Linux, |key| (key == "HOME")
            .then(|| OsString::from("/home/user")))
        .unwrap(),
        PathBuf::from("/home/user/.local/share/wrix/sessions")
    );
}

#[test]
fn omniwm_cli_resolution_does_not_require_shell_path() {
    let bundled = PathBuf::from("/Applications/OmniWM.app/Contents/MacOS/omniwmctl");
    assert_eq!(
        process::executable("omniwmctl", |_| None, |path| path == bundled),
        bundled
    );
    assert_eq!(
        process::executable(
            "omniwmctl",
            |key| (key == "HOME").then(|| "/Users/user".into()),
            |path| path.starts_with("/Users/user")
        ),
        PathBuf::from("/Users/user/Applications/OmniWM.app/Contents/MacOS/omniwmctl")
    );
    assert_eq!(
        process::executable(
            "omniwmctl",
            |key| (key == "WRIX_OMNIWMCTL").then(|| "/custom/omniwmctl".into()),
            |_| true
        ),
        PathBuf::from("/custom/omniwmctl")
    );
}

#[test]
fn subprocess_keeps_json_errors_written_to_stdout() {
    assert!(matches!(process::execute(Command::new("sh").args(["-c",
        r#"printf '%s' '{"ok":false,"code":"stale_window_id"}'; exit 1"#]),
        Duration::from_secs(3)), Err(Error::Status(_, output)) if output.contains("stale_window_id")));
}

#[test]
fn subprocess_timeout_kills_and_reaps_without_waiting_for_inherited_pipes() {
    let start = Instant::now();
    let error = process::execute(
        Command::new("sh").args(["-c", "sleep 1 & wait"]),
        Duration::from_millis(30),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Timeout));
    assert!(start.elapsed() < Duration::from_millis(900));
}

#[test]
fn subprocess_output_larger_than_pipe_capacity_is_drained() {
    let output = process::execute(
        Command::new("sh").args(["-c", "head -c 200000 /dev/zero | tr '\\000' x"]),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(output.len(), 200_000);
}

#[test]
fn subprocess_failures_and_invalid_utf8_are_errors() {
    assert!(
        matches!(process::execute(Command::new("sh").args(["-c","echo unavailable >&2; exit 72"]),Duration::from_secs(3)),Err(Error::Status(_,message)) if message.contains("unavailable"))
    );
    assert!(matches!(
        process::execute(
            Command::new("sh").args(["-c", "printf '\\377'"]),
            Duration::from_secs(3)
        ),
        Err(Error::Utf8(_))
    ));
}

#[test]
fn shortcut_command_failures_are_visible_without_debug_logging() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let directory = command::session_directory(Platform::host(), |name| match name {
        "HOME" | "XDG_RUNTIME_DIR" => Some(root.path().as_os_str().to_owned()),
        _ => None,
    })
    .unwrap();
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("target.json"), serde_json::json!({
        "focus_target":"target", "window_id":"1","linux_route":{"terminal_pid":10,"client":null},
        "omniwm_route":{"window":"ow_target","terminal_pid":10,"client":null}
    }).to_string()).unwrap();
    let backend = root.path().join("niri");
    fs::write(
        &backend,
        "#!/bin/sh\nprintf 'backend fixture failure' >&2\nexit 77\n",
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
    let configured = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wrix-focus-notification"));
        command
            .env("HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .env("PATH", root.path())
            .env("WRIX_OMNIWMCTL", &backend);
        command
    };
    assert!(
        configured()
            .args(["--enqueue", "target"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = configured().output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("backend fixture failure"), "{stderr}");
    assert!(stderr.contains("attention command failed"), "{stderr}");
}

#[test]
fn executable_help_and_status_codes() {
    let bin = env!("CARGO_BIN_EXE_wrix-focus-notification");
    assert!(
        Command::new(bin)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        Command::new(bin)
            .arg("--unknown")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    let runtime = tempfile::tempdir().unwrap();
    let output = Command::new(bin)
        .args(["--is-focused", "missing"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("XDG_DATA_HOME", runtime.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !runtime
            .path()
            .join("wrix/sessions/attention.sqlite")
            .exists()
    );
    assert!(
        Command::new(bin)
            .args(["--enqueue", "missing"])
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_DATA_HOME", runtime.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}
