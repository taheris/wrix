//! Actual packaged public CLI admission, supervision, Run PTY and Spawn lifecycle.
mod manager;
use super::*;
use manager::Manager;
use serde_json::{Value, json};
use std::process::{Child, Stdio};

#[test]
#[ignore = "requires actual packaged Linux Podman, pool KVM and private stock user supervision"]
fn podman_public_run_and_spawn_reuse_installed_store() {
    run_case(
        Platform::Podman,
        "nix_store::admission::request::native::platform::public::podman_public_run_and_spawn_reuse_installed_store",
        exercise,
        false,
    );
}
#[test]
#[ignore = "requires actual packaged independent krun VMs and private stock user supervision"]
fn krun_public_run_and_spawn_reuse_installed_store() {
    run_case(
        Platform::Krun,
        "nix_store::admission::request::native::platform::public::krun_public_run_and_spawn_reuse_installed_store",
        exercise,
        false,
    );
}
fn command(manager: &Manager, caller: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(std::env::var_os("WRIX_TEST_POOL_PUBLIC_CLI").unwrap());
    manager.configure(&mut command);
    command.current_dir(caller).args(args);
    command
}
fn invoke(manager: &Manager, caller: &Path, args: &[&str]) -> std::process::Output {
    command(manager, caller, args).output().unwrap()
}
fn success_cli(manager: &Manager, caller: &Path, args: &[&str]) {
    let output = invoke(manager, caller, args);
    assert!(
        output.status.success(),
        "public CLI {args:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn profile(control: &Path, next: bool) -> PathBuf {
    let source = std::env::var_os(if next {
        "WRIX_TEST_POOL_NEXT_PROFILE"
    } else {
        "WRIX_TEST_POOL_PROFILE"
    })
    .unwrap();
    let mut value = observation::read(Path::new(&source));
    value["services"]["nix_cache"]["enable"] = json!(false);
    value["resources"] = json!({"memory_mb":2048,"pids_limit":256,"cpus":2});
    value["profile"]["mounts"] = json!([{"source":control,"dest":"/control","mode":"rw"}]);
    let path = control.join(if next {
        "next-profile.json"
    } else {
        "public-profile.json"
    });
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    path
}
fn spawn_config(control: &Path, caller: &Path, name: &str) -> PathBuf {
    let path = control.join(format!("{name}-spawn.json"));
    fs::write(&path, serde_json::to_vec(&json!({"workspace":caller,"env":[],"agent_args":[format!("public-{name}")],"tool_caches":{"shared":false},"nix_store":{"mode":"shared"}})).unwrap()).unwrap();
    path
}
fn start(
    manager: &Manager,
    caller: &Path,
    control: &Path,
    profile: &Path,
    name: &str,
    run: bool,
) -> Child {
    let mut command = if run {
        let mut command = Command::new(
            std::env::var_os("WRIX_TEST_POOL_PYTHON").unwrap_or_else(|| "python3".into()),
        );
        manager.configure(&mut command);
        command
            .current_dir(caller)
            .arg(std::env::var_os("WRIX_TEST_POOL_PUBLIC_RUN").unwrap())
            .arg(profile);
        command
    } else {
        let config = spawn_config(control, caller, name);
        command(
            manager,
            caller,
            &[
                "--profile-config",
                profile.to_str().unwrap(),
                "spawn",
                "--spawn-config",
                config.to_str().unwrap(),
            ],
        )
    };
    let log = fs::File::create(control.join(format!("{name}-public-cli.log"))).unwrap();
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap()
}
fn milestone(control: &Path, name: &str, children: &mut [&mut Child]) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !control.join(name).is_file() {
        for child in children.iter_mut() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "public CLI exited before {name}; inspect {}",
                control.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "public milestone {name} timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn clients(host: &wrix_state::host::Host) -> Vec<Value> {
    fn visit(root: &Path, values: &mut Vec<Value>) {
        if !root.is_dir() {
            return;
        }
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, values);
            } else if path.file_name().unwrap() == "client.json" {
                values.push(observation::read(&path));
            }
        }
    }
    let mut values = Vec::new();
    visit(&host.roots().state().join("control/nix"), &mut values);
    values
}
fn released(host: &wrix_state::host::Host) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let values = clients(host);
        if !values.is_empty() && values.iter().all(|v| v["state"]["phase"] == "released") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "public client protection did not drain: {values:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn exercise(platform: Platform, caller: &Path) {
    assert_ne!(platform, Platform::Apple);
    let root = caller.parent().unwrap();
    let control = caller.join("native-control");
    fs::create_dir(&control).unwrap();
    let manager = Manager::start(root, platform);
    // This existing runtime selector chooses krun, never qualifies a transport.
    let profile = profile(&control, false);
    let next_profile = self::profile(&control, true);
    let linked = root.join("linked");
    let peer = root.join("peer");
    let host = wrix_state::host::Host::discover().unwrap();
    let denied = invoke(
        &manager,
        caller,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
            "--no-cache",
        ],
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("explicit host membership/approval"));
    success_cli(&manager, caller, &["service", "start", "--no-cache"]);
    for workspace in [caller, linked.as_path(), peer.as_path()] {
        success_cli(&manager, workspace, &["service", "nix-store", "enable"]);
    }
    let args = [
        "--profile-config",
        profile.to_str().unwrap(),
        "run",
        "--nix-store",
        "shared",
        "--no-cache",
    ];
    let denied = command(&manager, caller, &args)
        .env("WRIX_NETWORK", "limit")
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("require network=open"));
    let mut incompatible = observation::read(&profile);
    incompatible["image"]
        .as_object_mut()
        .unwrap()
        .remove("shared_nix");
    let incompatible_path = control.join("incompatible-profile.json");
    fs::write(
        &incompatible_path,
        serde_json::to_vec(&incompatible).unwrap(),
    )
    .unwrap();
    let denied = invoke(
        &manager,
        caller,
        &[
            "--profile-config",
            incompatible_path.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
            "--no-cache",
        ],
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("compatible versioned image"));
    assert!(
        !host.roots().state().join("control/nix").exists(),
        "public preflight provisioned a pool"
    );
    let invalid = spawn_config(&control, caller, "invalid");
    let failed = invoke(
        &manager,
        caller,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "spawn",
            "--spawn-config",
            invalid.to_str().unwrap(),
        ],
    );
    assert!(
        !failed.status.success(),
        "invalid real worker agent unexpectedly succeeded"
    );
    fs::write(control.join("launch-failure.stderr"), failed.stderr).unwrap();
    fs::write(control.join("launch-failure.stdout"), failed.stdout).unwrap();
    assert_eq!(
        observation::read(&control.join("invalid-agent-reached.json"))["intentional_exit"],
        23
    );
    assert_eq!(
        observation::read(&control.join("invalid-agent-store-info.json"))["trusted"],
        false
    );
    released(&host);
    // A real stopped controller cannot acknowledge readiness. Keep its owned
    // unit and all production checks, then resume the same process.
    let paused = manager::Paused::new(&manager);
    let failed = invoke(
        &manager,
        caller,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
            "--no-cache",
        ],
    );
    drop(paused);
    assert!(
        !failed.status.success(),
        "stopped controller granted public readiness"
    );
    fs::write(control.join("readiness-failure.stderr"), failed.stderr).unwrap();
    let outside = host.roots().state().join("public-world-readable-control");
    fs::write(&outside, b"outside public worker").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(
        control.join("outside-path"),
        outside.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let archive = fs::File::create(control.join("unsigned.nar")).unwrap();
    assert!(
        Command::new(std::env::var_os("WRIX_TEST_POOL_NIX_STORE").unwrap())
            .args(["--export"])
            .arg(std::env::var_os("WRIX_TEST_POOL_UNSIGNED").unwrap())
            .stdout(archive)
            .status()
            .unwrap()
            .success()
    );
    let mut first = start(&manager, caller, &control, &profile, "first", false);
    let mut second = start(&manager, &linked, &control, &profile, "second", true);
    milestone(&control, "first-ready.json", &mut [&mut first, &mut second]);
    milestone(
        &control,
        "second-ready.json",
        &mut [&mut first, &mut second],
    );
    observation::independent(
        platform,
        &observation::read(&control.join("first-ready.json")),
        &observation::read(&control.join("second-ready.json")),
    );
    let mut peer_worker = start(&manager, &peer, &control, &profile, "peer", false);
    milestone(&control, "peer-ready.json", &mut [&mut peer_worker]);
    fs::write(control.join("go"), b"go").unwrap();
    milestone(&control, "first-done.json", &mut [&mut first, &mut second]);
    milestone(&control, "second-done.json", &mut [&mut first, &mut second]);
    let first_done = observation::read(&control.join("first-done.json"));
    let second_done = observation::read(&control.join("second-done.json"));
    observation::workflows(&first_done, &second_done);
    let live_gc = invoke(
        &manager,
        caller,
        &["service", "nix-store", "gc", "--wait", "0s"],
    );
    assert!(!live_gc.status.success());
    assert!(String::from_utf8_lossy(&live_gc.stdout).contains("maintenance access remained busy"));
    first.kill().unwrap();
    assert!(!first.wait().unwrap().success());
    success_cli(&manager, caller, &["service", "nix-store", "disable"]);
    let denied = invoke(
        &manager,
        caller,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
            "--no-cache",
        ],
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("explicit host membership/approval"));
    assert!(
        manager
            .systemctl(&["restart", "wrix-host-controller.service"])
            .status
            .success()
    );
    fs::write(control.join("recheck"), b"go").unwrap();
    milestone(&control, "first-resumed.json", &mut [&mut second]);
    success_cli(&manager, caller, &["service", "nix-store", "enable"]);
    fs::write(control.join("second-exit"), b"exit").unwrap();
    assert!(second.wait().unwrap().success());
    let mut replacement = start(
        &manager,
        caller,
        &control,
        &next_profile,
        "replacement",
        false,
    );
    milestone(&control, "replacement-done.json", &mut [&mut replacement]);
    let replacement_done = observation::read(&control.join("replacement-done.json"));
    observation::workflows(&first_done, &replacement_done);
    for name in ["first", "replacement", "peer"] {
        fs::write(control.join(format!("{name}-exit")), b"exit").unwrap();
    }
    assert!(replacement.wait().unwrap().success());
    assert!(peer_worker.wait().unwrap().success());
    released(&host);
    success_cli(
        &manager,
        caller,
        &["service", "nix-store", "gc", "--wait", "30s"],
    );
    let report = observation::public_report(
        platform,
        &control,
        &[first_done, second_done, replacement_done],
    );
    fs::write(
        root.join("native-measurements.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    success_cli(&manager, caller, &["service", "stop", "--host"]);
}
