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
fn session_directory_preserves_platform_roots_and_home_fallback() {
    for (platform, name) in [
        (Platform::Linux, "XDG_RUNTIME_DIR"),
        (Platform::Darwin, "XDG_DATA_HOME"),
    ] {
        assert_eq!(
            command::session_directory(platform, |key| (key == name)
                .then(|| OsString::from("/runtime")))
            .unwrap(),
            PathBuf::from("/runtime/wrix/sessions")
        );
        assert_eq!(
            command::session_directory(platform, |key| (key == "HOME")
                .then(|| OsString::from("/home/user")))
            .unwrap(),
            PathBuf::from("/home/user/.local/share/wrix/sessions")
        );
    }
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
