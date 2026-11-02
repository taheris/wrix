use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

#[test]
fn nextest_setup_isolates_home_and_cache_without_mutating_the_callers_home() {
    let root = tempfile::tempdir().unwrap();
    let caller = root.path().join("caller-home");
    fs::create_dir(&caller).unwrap();
    fs::write(caller.join("retained"), b"untouched").unwrap();
    let environment = root.path().join("environment");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/prepare-isolated-host-roots.sh");
    let output = Command::new("bash")
        .arg(script)
        .env("HOME", &caller)
        .env("NEXTEST_ENV", &environment)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = fs::read_to_string(environment).unwrap();
    let values: BTreeMap<_, _> = text
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    let home = Path::new(values["HOME"]);
    assert_ne!(home, caller);
    assert!(home.is_dir());
    assert_eq!(home.canonicalize().unwrap(), home);
    assert_eq!(
        fs::metadata(home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(values["XDG_STATE_HOME"], "");
    assert_eq!(values["TOKIO_WORKER_THREADS"], "2");
    assert_eq!(Path::new(values["XDG_CACHE_HOME"]), home.parent().unwrap());
    assert_eq!(
        Path::new(values["TMPDIR"]),
        fs::canonicalize("/tmp").unwrap()
    );
    assert_eq!(fs::read(caller.join("retained")).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(caller).unwrap().count(), 1);
}
