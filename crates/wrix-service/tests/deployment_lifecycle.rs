use std::{
    fs,
    process::{Command, ExitCode},
};
use wrix_cli::command;

#[test]
fn public_cli_update_rejects_selectors_before_workspace_or_runtime_dispatch() {
    for selector in [
        "--host",
        "--force",
        "--wait",
        "--no-cache",
        "gateway",
        "pool",
        "image",
    ] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let error = command::run(
            &["service".into(), "update".into(), selector.into()],
            &mut stdout,
            &mut stderr,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{selector}");
        assert_eq!(stdout, [] as [u8; 0]);
        assert_eq!(stderr, [] as [u8; 0]);
    }
}

#[test]
fn public_cli_host_shutdown_rejects_workspace_options_before_dispatch() {
    for extra in ["--host", "--no-cache", "--sandbox-cache", "workspace"] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(
            command::run(
                &[
                    "service".into(),
                    "stop".into(),
                    "--host".into(),
                    extra.into()
                ],
                &mut stdout,
                &mut stderr
            )
            .is_err()
        );
        assert_eq!(stdout, [] as [u8; 0]);
    }
}

#[test]
fn public_cli_deployment_dispatch_is_host_only_and_never_bootstraps_an_absent_deployment() {
    const CHILD: &str = "WRIX_TEST_DEPLOYMENT_DISPATCH";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(root);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let update = command::run(
            &["service".into(), "update".into()],
            &mut stdout,
            &mut stderr,
        )
        .unwrap_err();
        let worker = std::path::Path::new("/etc/wrix/image-agent").exists();
        assert!(
            update.to_string().contains(if worker {
                "host-only"
            } else {
                "existing deployment"
            }),
            "{update}"
        );
        assert_eq!(stdout, [] as [u8; 0]);
        let stop = command::run(
            &["service".into(), "stop".into(), "--host".into()],
            &mut stdout,
            &mut stderr,
        );
        if worker {
            assert!(stop.unwrap_err().to_string().contains("host-only"));
        } else {
            assert_eq!(stop.unwrap(), ExitCode::SUCCESS);
            assert!(
                String::from_utf8(stdout)
                    .unwrap()
                    .contains("deployment: absent")
            );
        }
        assert!(!root.join("state").exists());
        assert!(!root.join("home/Library/Application Support/wrix").exists());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "public_cli_deployment_dispatch_is_host_only_and_never_bootstraps_an_absent_deployment",
            "--nocapture",
        ])
        .current_dir(root.path())
        .env(CHILD, root.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("WRIX_SERVICE_PACKAGE_BINDING", "/nonexistent/binding.json")
        .env_remove("WRIX_PROJECT_PROFILE_CONFIG")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
