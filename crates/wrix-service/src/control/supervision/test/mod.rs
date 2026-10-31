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
fn retained_unit_accepts_only_canonical_host_runtime_environment() {
    let root = Path::new("/private/controller");
    let package = package("blue");
    let base = unit_contents(root, &package);
    assert!(unit_matches(root, &package, &base));
    let environment = format!(
        "Environment={}\n",
        systemd_path(Path::new("WRIX_CONTAINER_RUNTIME=/host/podman%wrapper"))
    );
    assert!(unit_matches(
        root,
        &package,
        &format!("{base}{environment}")
    ));
    for extra in [
        format!("{environment}{environment}"),
        "ExecStart=/foreign/program\n".into(),
        "Environment=WRIX_CONTAINER_RUNTIME=/foreign/program\n".into(),
        format!(
            "Environment={}\n",
            systemd_path(Path::new("LD_PRELOAD=/foreign/library"))
        ),
        format!("{environment}Environment=\"\\x57\"\n"),
    ] {
        assert!(!unit_matches(root, &package, &format!("{base}{extra}")));
    }
    let mut foreign = package;
    foreign.controller = "/nix/store/green-controller".into();
    assert!(!unit_matches(root, &foreign, &base));
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

fn linux_manager(directory: &Path, state: &str) -> Native {
    fs::write(directory.join("state"), state).unwrap();
    let supervisor = script(
        directory,
        "systemctl",
        &format!(
            r#"
printf '%s\n' "$*" >> '{root}/commands'
case "$2" in
    link)
        if [[ "$3" == --force ]]; then
            ln -sfn -- "$4" '{root}/linked'
        elif [[ -L '{root}/linked' ]]; then
            [[ "$(readlink '{root}/linked')" == "$3" ]] || exit 1
        else
            ln -s -- "$3" '{root}/linked'
        fi
        ;;
    show) < '{root}/state' head -c 4096 ;;
    daemon-reload|start) ;;
    *) exit 91 ;;
esac
"#,
            root = directory.display()
        ),
    );
    Native::new(Platform::Linux, 1000, supervisor, "/unused".into())
}

#[test]
fn linux_start_recovers_a_dangling_unit_link() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::os::unix::fs::symlink(root.join("deleted/controller.service"), root.join("linked"))
        .unwrap();
    let native = linux_manager(
        root,
        "LoadState=not-found\nActiveState=inactive\nFragmentPath=\n",
    );
    native.start(root, &package("blue")).unwrap();
    native.start(root, &package("blue")).unwrap();
    assert_eq!(
        fs::read_link(root.join("linked")).unwrap(),
        root.join("wrix-host-controller.service")
    );
    let commands = fs::read_to_string(root.join("commands")).unwrap();
    assert_eq!(commands.matches("--user link --force ").count(), 1);
    assert_eq!(
        commands
            .matches("--user start wrix-host-controller.service")
            .count(),
        2
    );
    assert!(!commands.contains("stop"));
}

#[test]
fn linux_controller_unit_does_not_kill_export_helpers_with_the_control_process() {
    // Verify the emitted unit contract, not actual service-manager restart acceptance.
    let directory = tempfile::tempdir().unwrap();
    let native = linux_manager(
        directory.path(),
        "LoadState=not-found\nActiveState=inactive\nFragmentPath=\n",
    );
    native.start(directory.path(), &package("blue")).unwrap();
    let unit = fs::read_to_string(directory.path().join("wrix-host-controller.service")).unwrap();
    assert_eq!(
        unit.lines()
            .filter(|line| line.starts_with("KillMode="))
            .collect::<Vec<_>>(),
        ["KillMode=process"]
    );
    assert!(unit.lines().any(|line| line == "Restart=on-failure"));
}

#[test]
fn linux_start_preserves_existing_or_uncertain_controller_units() {
    for state in [
        "LoadState=loaded\nActiveState=active\nFragmentPath=/other/controller.service\n",
        "LoadState=loaded\nActiveState=inactive\nFragmentPath=/other/controller.service\n",
        "LoadState=not-found\nActiveState=active\nFragmentPath=\n",
        "LoadState=masked\nActiveState=inactive\nFragmentPath=/dev/null\n",
        "LoadState=not-found\nFragmentPath=\n",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let existing = root.join("other/controller.service");
        std::os::unix::fs::symlink(&existing, root.join("linked")).unwrap();
        let native = linux_manager(root, state);
        assert!(matches!(
            native.start(root, &package("blue")),
            Err(Error::UnitConflict(_))
        ));
        assert_eq!(fs::read_link(root.join("linked")).unwrap(), existing);
        let commands = fs::read_to_string(root.join("commands")).unwrap();
        assert!(!commands.contains("--force"));
        assert!(!commands.contains("daemon-reload"));
        assert!(!commands.contains("start"));
        assert!(!commands.contains("stop"));
    }
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
        assert!(serde_json::from_slice::<Package>(&fs::read(&manifest).unwrap()).is_err());
    }
}

fn upgrade_manager(directory: &Path, root: &Path, foreign: bool) -> Native {
    let fragment = if foreign {
        directory.join("foreign.service")
    } else {
        root.join("wrix-host-controller.service")
    };
    upgrade_manager_fragment(directory, &fragment)
}

fn upgrade_manager_fragment(directory: &Path, fragment: &Path) -> Native {
    let supervisor = script(
        directory,
        "upgrade-systemctl",
        &format!(
            r#"
printf '%s\n' "$*" >> '{directory}/upgrade-commands'
case "$2" in
  show-environment|link|daemon-reload) ;;
  show) printf '%s\n' LoadState=loaded ActiveState=active 'FragmentPath={fragment}' ;;
  stop) touch '{directory}/native-stopped' ;;
  start) touch '{directory}/native-started' ;;
  *) exit 91 ;;
esac
"#,
            directory = directory.display(),
            fragment = fragment.display()
        ),
    );
    let pin = script(directory, "upgrade-nix-store", "exit 0");
    Native::new(Platform::Linux, 1000, supervisor, pin)
}

#[test]
fn explicit_upgrade_stops_verified_old_unit_before_migrating_then_selects_new_package() {
    let directory = tempfile::tempdir().unwrap();
    let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
    let old = package("old");
    let new = package("new");
    fs::write(
        lifecycle.root.join("package.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    fs::write(
        lifecycle.root.join("wrix-host-controller.service"),
        unit_contents(&lifecycle.root, &old),
    )
    .unwrap();
    fs::write(lifecycle.root.join("retained-key"), b"private authority").unwrap();
    let native = upgrade_manager(directory.path(), &lifecycle.root, false);
    lifecycle
        .upgrade(&native, &new, || {
            assert!(directory.path().join("native-stopped").exists());
            assert!(!directory.path().join("native-started").exists());
            assert_eq!(
                Package::load(&lifecycle.root.join("package.json")).unwrap(),
                old
            );
            fs::write(lifecycle.root.join("migrated"), b"v2 policy")
        })
        .unwrap();
    assert_eq!(
        Package::load(&lifecycle.root.join("package.json")).unwrap(),
        new
    );
    assert_eq!(
        fs::read(lifecycle.root.join("retained-key")).unwrap(),
        b"private authority"
    );
    assert!(directory.path().join("native-started").exists());
    assert!(lifecycle.root.join("migrated").exists());
    assert!(!lifecycle.root.join("stopped").exists());
}

#[test]
fn explicit_upgrade_accepts_systemd_user_link_but_rejects_identical_foreign_units() {
    for foreign in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
        let old = package("old");
        let record = serde_json::to_vec(&old).unwrap();
        fs::write(lifecycle.root.join("package.json"), &record).unwrap();
        let unit = lifecycle.root.join("wrix-host-controller.service");
        fs::write(&unit, unit_contents(&lifecycle.root, &old)).unwrap();
        let user_units = directory.path().join("user config/systemd/user");
        fs::create_dir_all(&user_units).unwrap();
        let fragment = user_units.join("wrix-host-controller.service");
        if foreign {
            let copy = directory.path().join("foreign.service");
            fs::copy(&unit, &copy).unwrap();
            std::os::unix::fs::symlink(copy, &fragment).unwrap();
        } else {
            std::os::unix::fs::symlink(&unit, &fragment).unwrap();
        }
        let native = upgrade_manager_fragment(directory.path(), &fragment);
        let result = lifecycle.upgrade(&native, &package("new"), || Ok(()));
        if foreign {
            assert!(matches!(result, Err(Error::UnitConflict(_))));
            assert_eq!(
                fs::read(lifecycle.root.join("package.json")).unwrap(),
                record
            );
        } else {
            result.unwrap();
            assert_eq!(
                Package::load(&lifecycle.root.join("package.json")).unwrap(),
                package("new")
            );
        }
        assert_eq!(directory.path().join("native-stopped").exists(), !foreign);
        assert_eq!(directory.path().join("native-started").exists(), !foreign);
    }
}

#[test]
fn explicit_upgrade_never_stops_foreign_units_or_replaces_package_after_failed_migration() {
    for foreign in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
        let old = package("old");
        let record = serde_json::to_vec(&old).unwrap();
        fs::write(lifecycle.root.join("package.json"), &record).unwrap();
        fs::write(
            lifecycle.root.join("wrix-host-controller.service"),
            unit_contents(&lifecycle.root, &old),
        )
        .unwrap();
        let native = upgrade_manager(directory.path(), &lifecycle.root, foreign);
        assert!(
            lifecycle
                .upgrade(&native, &package("new"), || Err(io::Error::other(
                    "ownership still held"
                )))
                .is_err()
        );
        assert_eq!(
            fs::read(lifecycle.root.join("package.json")).unwrap(),
            record
        );
        assert_eq!(directory.path().join("native-stopped").exists(), !foreign);
        assert!(!directory.path().join("native-started").exists());
    }
}

#[test]
fn explicit_host_stop_preserves_package_and_roots_after_native_stop_failure_and_retry() {
    let directory = tempfile::tempdir().unwrap();
    let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
    let old = package("recorded");
    let bytes = serde_json::to_vec(&old).unwrap();
    fs::write(lifecycle.root.join("package.json"), &bytes).unwrap();
    fs::write(
        lifecycle.root.join("wrix-host-controller.service"),
        unit_contents(&lifecycle.root, &old),
    )
    .unwrap();
    fs::create_dir(lifecycle.root.join("roots")).unwrap();
    std::os::unix::fs::symlink(
        "/nix/store/recorded-controller",
        lifecycle.root.join("roots/controller"),
    )
    .unwrap();
    let native = upgrade_manager(directory.path(), &lifecycle.root, false);
    let original = fs::read_to_string(&native.supervisor).unwrap();
    fs::write(
        &native.supervisor,
        original.replace("touch", "exit 73; touch"),
    )
    .unwrap();
    assert!(lifecycle.stop_owned(&native).is_err());
    assert!(lifecycle.root.join("stopped").is_file());
    assert_eq!(
        fs::read(lifecycle.root.join("package.json")).unwrap(),
        bytes
    );
    fs::write(&native.supervisor, original).unwrap();
    lifecycle.stop_owned(&native).unwrap();
    assert!(directory.path().join("native-stopped").exists());
    assert_eq!(
        fs::read_link(lifecycle.root.join("roots/controller")).unwrap(),
        Path::new("/nix/store/recorded-controller")
    );
    assert!(!directory.path().join("native-started").exists());
}

#[test]
fn explicit_host_stop_refuses_foreign_units_before_recording_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let lifecycle = Lifecycle::lock(&directory.path().join("controller")).unwrap();
    let old = package("recorded");
    fs::write(
        lifecycle.root.join("package.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    fs::write(
        lifecycle.root.join("wrix-host-controller.service"),
        unit_contents(&lifecycle.root, &old),
    )
    .unwrap();
    let native = upgrade_manager(directory.path(), &lifecycle.root, true);
    assert!(matches!(
        lifecycle.stop_owned(&native),
        Err(Error::UnitConflict(_))
    ));
    assert!(!lifecycle.root.join("stopped").exists());
    assert!(!directory.path().join("native-stopped").exists());
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
