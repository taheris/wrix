#![cfg(test)]

#[path = "execution_fixtures/process/mod.rs"]
mod process;

use process::{Owned, run};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/security")
        .join(name)
}

fn equivalent(actual: Output, expected: Output) {
    assert_eq!(
        (actual.status, actual.stdout, actual.stderr),
        (expected.status, expected.stdout, expected.stderr)
    );
}

fn runner() -> std::ffi::OsString {
    std::env::var_os("WRIX_EXECUTION_FIXTURE_RUNNER")
        .expect("owning fixture runner must be provided")
}

#[test]
fn non_run_runtime_preserves_real_command_streams_environment_and_status() {
    let root = tempfile::tempdir().unwrap();
    let args = [
        "-c",
        "IFS= read -r line; printf '%s:%s:%s:%s\\n' \"$1\" \"$line\" \"$HOME\" \"$XDG_CACHE_HOME\"; echo diagnostic >&2; exit 37",
        "fixture",
        "argument with spaces",
    ];
    let configure = |command: &mut Command| {
        command
            .args(args)
            .env("HOME", root.path())
            .env("XDG_CACHE_HOME", root.path().join("cache"));
    };
    let mut expected = Command::new("bash");
    configure(&mut expected);
    let mut actual = Command::new("bash");
    actual
        .arg(fixture("execution-runtime.sh"))
        .env("WRIX_TEST_REAL_RUNTIME", "bash");
    configure(&mut actual);
    equivalent(run(&mut actual, b"input\n"), run(&mut expected, b"input\n"));
}

#[test]
fn runtime_gate_names_then_executes_delegate_without_mutating_other_arguments() {
    let root = tempfile::tempdir().unwrap();
    let bash = run(
        Command::new("bash").args(["-c", "printf '%s' \"$BASH\""]),
        b"",
    );
    assert!(bash.status.success());
    let delegate = root.path().join("delegate");
    fs::write(&delegate, format!("#!{}\nset -euo pipefail\nprintf '%s\\0' \"$@\"\ncat\nprintf 'runtime diagnostic\\n' >&2\nexit 37\n", String::from_utf8(bash.stdout).unwrap())).unwrap();
    fs::set_permissions(&delegate, fs::Permissions::from_mode(0o755)).unwrap();
    let expected = run(
        Command::new(&delegate).args([
            "run",
            "--name",
            "fixture-name",
            "--rm",
            "image:tag",
            "space argument",
            "",
        ]),
        b"stream\n",
    );
    let child = Owned::spawn(
        Command::new("bash")
            .arg(fixture("execution-runtime.sh"))
            .args(["run", "--rm", "image:tag", "space argument", ""])
            .env("WRIX_TEST_REAL_RUNTIME", &delegate)
            .env("WRIX_TEST_CONTROL", root.path())
            .env("WRIX_TEST_CONTAINER_NAME", "fixture-name"),
    );
    child.ready(&root.path().join("ready"));
    assert_eq!(
        fs::read_to_string(root.path().join("host-pid"))
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap(),
        std::process::id()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("runtime-pid"))
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap(),
        child.id()
    );
    fs::write(root.path().join("release"), "").unwrap();
    equivalent(child.finish(b"stream\n"), expected);
}

#[test]
#[ignore = "requires the actual Nix-packaged execution fixture runner"]
fn packaged_runner_preserves_consumer_arguments_streams_and_status() {
    let args = [
        "bash",
        "-c",
        "IFS= read -r line; printf '%s|%s|%s\\n' \"$1\" \"$2\" \"$line\"; echo diagnostic >&2; exit 37",
        "consumer",
        "two words",
        "",
    ];
    equivalent(
        run(Command::new(runner()).args(args), b"input\n"),
        run(Command::new(args[0]).args(&args[1..]), b"input\n"),
    );
}

#[test]
#[ignore = "requires the actual Nix-packaged execution fixture runner"]
fn packaged_runner_preserves_probe_release_output_and_exit() {
    let root = tempfile::tempdir().unwrap();
    for status in [0, 37] {
        let member = format!("member-{status}");
        let args = [
            fixture("execution-probe.sh").into_os_string(),
            root.path().as_os_str().to_owned(),
            member.clone().into(),
            status.to_string().into(),
        ];
        let child = Owned::spawn(Command::new(runner()).arg("bash").args(&args));
        child.ready(&root.path().join(format!("ready-{member}")));
        fs::write(root.path().join(format!("exit-{member}")), "").unwrap();
        equivalent(
            child.finish(b""),
            run(Command::new("bash").args(&args), b""),
        );
    }
}
