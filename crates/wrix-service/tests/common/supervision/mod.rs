use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use wrix_service::control::supervision::{Error, Lifecycle, Native, Package, Platform};

const CHILD: &str = "WRIX_TEST_SUPERVISION_CHILD";

fn script(path: &Path, body: &str) {
    fs::write(
        path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture coordination timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

pub fn exercise() {
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let package = Package::from_env().unwrap();
        let lifecycle = Lifecycle::lock(&root.join("controller")).unwrap();
        let native = Native::new(
            Platform::Linux,
            1000,
            root.join("tools/systemctl"),
            root.join("tools/nix-store"),
        );
        let action = std::env::var("WRIX_TEST_ACTION").unwrap();
        match action.as_str() {
            "start" => {
                lifecycle.start(&native, &package).unwrap();
            }
            "stop" => {
                lifecycle.stop(&native).unwrap();
            }
            "failed-stop" => {
                assert!(matches!(lifecycle.stop(&native), Err(Error::Supervisor(_))));
            }
            "missing" => {
                assert!(matches!(
                    lifecycle.start(&native, &package),
                    Err(Error::Supervisor(_))
                ));
            }
            "lock" => {
                fs::write(root.join("locked"), "").unwrap();
                wait_for(&root.join("release"));
            }
            _ => panic!("unknown test action"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let tools = root.join("tools");
    fs::create_dir(&tools).unwrap();
    script(
        &tools.join("systemctl"),
        r#"
printf '%s\n' "$*" >> "$WRIX_TEST_SUPERVISION_CHILD/commands"
[[ ! -f "$WRIX_TEST_SUPERVISION_CHILD/unavailable" ]]
[[ "$1" == --user ]]
case "$2" in
show-environment|link|daemon-reload|start|stop) ;;
*) exit 90 ;;
esac
"#,
    );
    script(
        &tools.join("nix-store"),
        r#"
[[ "$#" == 5 && "$1" == --realise && "$3" == --add-root && "$5" == --indirect ]]
printf '%s\n' "$2" >> "$WRIX_TEST_SUPERVISION_CHILD/closures"
ln -sfn "${2%.drv}" "$4"
"#,
    );
    let ambient = root.join("ambient-tools");
    fs::create_dir(&ambient).unwrap();
    for name in ["systemctl", "nix-store"] {
        script(&ambient.join(name), "exit 97");
    }
    let path = std::env::join_paths(
        [ambient, tools.clone()]
            .into_iter()
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let child = |action: &str| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "supervision_preserves_stop_intent_and_active_closures",
                "--nocapture",
            ])
            .env(CHILD, &root)
            .env(
                "WRIX_CONTROLLER_PACKAGE_BINDING",
                root.join("candidate.json"),
            )
            .env("WRIX_TEST_ACTION", action)
            .env("PATH", &path);
        command
    };
    let candidate = |color: &str| serde_json::json!({"controller":format!("/nix/store/{color}-controller"), "runtime":format!("/nix/store/{color}-runtime"), "image":format!("/nix/store/{color}-image.drv")});
    fs::write(
        root.join("candidate.json"),
        serde_json::to_vec(&candidate("blue")).unwrap(),
    )
    .unwrap();
    let mut a = child("start").spawn().unwrap();
    let mut b = child("start").spawn().unwrap();
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    let selected = root.join("controller/package.json");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&selected).unwrap()).unwrap(),
        candidate("blue")
    );
    for name in ["controller", "runtime", "image"] {
        let expected = if name == "image" {
            "/nix/store/blue-image".to_owned()
        } else {
            format!("/nix/store/blue-{name}")
        };
        assert_eq!(
            fs::read_link(root.join("controller/roots").join(name)).unwrap(),
            PathBuf::from(expected)
        );
    }
    let commands = fs::read_to_string(root.join("commands")).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|line| line.starts_with("--user start "))
            .count(),
        2
    );
    assert!(commands.lines().all(|line| line.starts_with("--user ")));
    assert!(child("stop").status().unwrap().success());
    assert!(root.join("controller/stopped").exists());
    let stopped = Command::new(env!("CARGO_BIN_EXE_wrix-host-controller"))
        .arg(root.join("controller"))
        .status()
        .unwrap();
    assert!(stopped.success());
    assert!(!root.join("controller/authority").exists());
    fs::write(
        root.join("candidate.json"),
        serde_json::to_vec(&candidate("green")).unwrap(),
    )
    .unwrap();
    assert!(child("start").status().unwrap().success());
    assert!(!root.join("controller/stopped").exists());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&selected).unwrap()).unwrap(),
        candidate("blue")
    );
    assert!(
        !fs::read_to_string(root.join("closures"))
            .unwrap()
            .contains("green")
    );
    assert!(child("stop").status().unwrap().success());
    fs::write(root.join("unavailable"), "").unwrap();
    assert!(child("failed-stop").status().unwrap().success());
    assert!(child("missing").status().unwrap().success());
    assert!(root.join("controller/stopped").exists());
    let before = fs::read(root.join("closures")).unwrap();
    fs::remove_file(tools.join("systemctl")).unwrap();
    let mut missing = child("missing");
    missing.env("PATH", &tools);
    assert!(missing.status().unwrap().success());
    assert_eq!(fs::read(root.join("closures")).unwrap(), before);
    let mut owner = child("lock").spawn().unwrap();
    wait_for(&root.join("locked"));
    let mut waiter = child("missing").spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(waiter.try_wait().unwrap().is_none());
    owner.kill().unwrap();
    assert!(!owner.wait().unwrap().success());
    assert!(waiter.wait().unwrap().success());
}
