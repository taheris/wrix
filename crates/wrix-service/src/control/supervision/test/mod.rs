use super::*;
use std::os::unix::fs::PermissionsExt;

fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(
        &path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn package(name: &str) -> Package {
    Package {
        controller: format!("/nix/store/{name}-controller").into(),
        runtime: format!("/nix/store/{name}-runtime").into(),
        image: format!("/nix/store/{name}-image.drv").into(),
    }
}

#[test]
fn darwin_bootstrap_and_loaded_kickstart_do_not_replace_running_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("log");
    let loaded = directory.path().join("loaded");
    let supervisor = script(
        directory.path(),
        "launchctl",
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\nprint) if [[ \"$2\" == */org.wrix.host-controller ]]; then [[ -f '{}' ]]; fi ;;\nbootstrap) touch '{}' ;;\nbootout) rm '{}' ;;\nesac",
            log.display(),
            loaded.display(),
            loaded.display(),
            loaded.display()
        ),
    );
    let native = Native {
        platform: Platform::Darwin,
        supervisor,
        nix_store: "/unused".into(),
        uid: 501,
    };
    native.check().unwrap();
    native.start(directory.path(), &package("blue")).unwrap();
    native.start(directory.path(), &package("blue")).unwrap();
    let observed = fs::read_to_string(log).unwrap();
    assert_eq!(
        observed
            .lines()
            .filter(|line| line.starts_with("bootstrap "))
            .count(),
        1
    );
    assert_eq!(
        observed
            .lines()
            .filter(|line| line.starts_with("kickstart "))
            .count(),
        1
    );
    assert!(!observed.contains("kickstart -k"));
    native.stop().unwrap();
    assert!(!loaded.exists());
    let plist =
        fs::read_to_string(directory.path().join("org.wrix.host-controller.plist")).unwrap();
    assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
    assert!(plist.contains("/nix/store/blue-controller/bin/wrix-host-controller"));
}

#[test]
fn root_failure_never_changes_selection_or_stop_intent() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = script(directory.path(), "supervisor", "exit 0");
    let nix_store = script(directory.path(), "nix-store", "exit 19");
    let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
    lifecycle
        .stop(&Native {
            platform: Platform::Linux,
            supervisor: supervisor.clone(),
            nix_store: nix_store.clone(),
            uid: 1000,
        })
        .unwrap();
    assert!(matches!(
        lifecycle.start(
            &Native {
                platform: Platform::Linux,
                supervisor,
                nix_store,
                uid: 1000
            },
            &package("blue")
        ),
        Err(Error::Root(_))
    ));
    assert!(lifecycle.root.join("stopped").exists());
    assert!(!lifecycle.root.join("package.json").exists());
}

#[test]
fn hostile_package_paths_are_rejected_before_supervision() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("package.json");
    for controller in [
        "/bin/sh",
        "/nix/store/../sh",
        "/nix/store/a/bin/sh",
        "relative",
    ] {
        fs::write(&manifest, serde_json::to_vec(&serde_json::json!({"controller":controller, "runtime":"/nix/store/runtime", "image":"/nix/store/image.drv"})).unwrap()).unwrap();
        assert!(matches!(Package::load(&manifest), Err(Error::Package)));
    }
}

#[test]
fn unit_paths_escape_specifiers_and_shell_metacharacters_without_a_shell() {
    assert_eq!(
        systemd_path(Path::new("/a% b\"")),
        "\"\\x2f\\x61%%\\x20\\x62\\x22\""
    );
    assert_eq!(
        xml(Path::new("/a<&\"'")).unwrap(),
        "/a&lt;&amp;&quot;&apos;"
    );
}
