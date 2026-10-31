use super::*;

fn script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn info(version: &str, rootless: bool, graph: &str, run: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": {"Version": version}, "host": {"security": {"rootless": rootless}},
        "store": {"graphRoot": graph, "runRoot": run}
    }))
    .unwrap()
}

#[test]
fn configured_system_runtime_precedes_devshell_path() {
    let root = tempfile::tempdir().unwrap();
    let host = script(root.path(), "host-podman", "exit 0");
    script(root.path(), "podman", "exit 125");
    let selected = resolve(
        Engine::Podman,
        None,
        Some(root.path().as_os_str()),
        std::slice::from_ref(&host),
    )
    .unwrap();
    assert_eq!(selected, host);
}

#[test]
fn explicit_selection_overrides_system_and_never_falls_back_when_missing() {
    let root = tempfile::tempdir().unwrap();
    let host = script(root.path(), "host-podman", "exit 0");
    let selected = script(root.path(), "selected-podman", "exit 0");
    assert_eq!(
        resolve(
            Engine::Podman,
            Some(selected.as_os_str()),
            None,
            std::slice::from_ref(&host)
        )
        .unwrap(),
        selected
    );
    assert!(
        resolve(
            Engine::Podman,
            Some(OsStr::new("missing")),
            Some(root.path().as_os_str()),
            &[host]
        )
        .is_err()
    );
}

#[test]
fn path_resolution_skips_nonexecutables_and_directories() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("podman")).unwrap();
    assert!(resolve(Engine::Podman, None, Some(root.path().as_os_str()), &[]).is_err());
    fs::remove_dir(root.path().join("podman")).unwrap();
    fs::write(root.path().join("podman"), "not executable").unwrap();
    assert!(resolve(Engine::Podman, None, Some(root.path().as_os_str()), &[]).is_err());
}

#[test]
fn readiness_accepts_supported_rootless_storage_without_requiring_default_locations() {
    for version in ["5.8.9", "6.1.4"] {
        validate_info(&info(
            version,
            true,
            "/srv/shaun/containers",
            "/run/user/1000/containers",
        ))
        .unwrap();
    }
}

#[test]
fn readiness_refuses_rootful_system_storage_and_unknown_runtime_versions() {
    for (version, rootless, graph, run) in [
        ("6.1.4", false, "/home/test/store", "/run/user/1000/store"),
        (
            "6.1.4",
            true,
            "/var/lib/containers/storage",
            "/run/user/1000/store",
        ),
        ("6.1.4", true, "/home/test/store", "/run/containers/storage"),
        ("7.0.0", true, "/home/test/store", "/run/user/1000/store"),
        ("6.1.4", true, "relative", "/run/user/1000/store"),
    ] {
        assert!(validate_info(&info(version, rootless, graph, run)).is_err());
    }
    assert!(validate_info(b"{}").is_err());
}

#[test]
fn readiness_timeout_is_bounded_even_when_a_descendant_holds_output_open() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 1 & wait"]);
    let started = Instant::now();
    assert!(capture(command, Duration::from_millis(50)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn selected_runtime_failure_names_the_executable_and_does_not_try_path_fallback() {
    const CHILD: &str = "WRIX_TEST_RUNTIME_CHILD";
    if env::var_os(CHILD).is_some() {
        let error = preflight().unwrap_err().to_string();
        assert!(error.contains(&env::var("WRIX_CONTAINER_RUNTIME").unwrap()));
        assert!(error.contains("6.1.4"));
        assert!(error.contains("permission denied"));
        assert!(error.contains("will not try another runtime"));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let selected = script(
        root.path(),
        "selected-podman",
        "case \"$1\" in --version) printf 'podman version 6.1.4\\n';; info) printf 'permission denied creating /var/lib/containers/storage/libpod\\n' >&2; exit 125;; *) exit 64;; esac",
    );
    let marker = root.path().join("fallback");
    script(
        root.path(),
        "podman",
        "printf 'called' > \"$WRIX_TEST_FALLBACK\"; exit 0",
    );
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", "container_runtime::test::selected_runtime_failure_names_the_executable_and_does_not_try_path_fallback", "--nocapture"])
        .env(CHILD, "1").env("WRIX_CONTAINER_RUNTIME", selected)
        .env("PATH", root.path()).env("WRIX_TEST_FALLBACK", &marker)
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists());
}

#[test]
fn process_selection_remains_canonical_after_host_symlink_changes() {
    const CHILD: &str = "WRIX_TEST_RUNTIME_FROZEN";
    if let Some(root) = env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let original = Engine::Podman.path().unwrap();
        fs::remove_file(root.join("selected")).unwrap();
        std::os::unix::fs::symlink(root.join("replacement"), root.join("selected")).unwrap();
        assert_eq!(Engine::Podman.path().unwrap(), original);
        let mut child = Command::new("unused");
        inherit(&mut child).unwrap();
        assert!(
            child
                .get_envs()
                .any(|(name, value)| name == "WRIX_CONTAINER_RUNTIME"
                    && value == Some(original.as_os_str()))
        );
        assert!(
            Engine::Podman
                .command()
                .unwrap()
                .status()
                .unwrap()
                .success()
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let original = script(root.path(), "original", "exit 0");
    script(root.path(), "replacement", "exit 125");
    std::os::unix::fs::symlink(original, root.path().join("selected")).unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", "container_runtime::test::process_selection_remains_canonical_after_host_symlink_changes", "--nocapture"])
        .env(CHILD, root.path()).env("WRIX_CONTAINER_RUNTIME", root.path().join("selected"))
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
