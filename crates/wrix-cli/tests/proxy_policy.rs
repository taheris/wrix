use std::{fs, process::Command};

use serde_json::json;

#[test]
fn missing_empty_or_invalid_ports_fail_before_preparation() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile.json");
    let spawn = root.path().join("spawn.json");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(
        &spawn,
        json!({"workspace": workspace, "env": [], "agent_args": []}).to_string(),
    )
    .unwrap();
    let kind = if cfg!(target_os = "macos") {
        "docker-archive"
    } else {
        "nix-descriptor"
    };
    for rule in [
        json!("example.org"),
        json!("example.org:443"),
        json!({"host": "example.org"}),
        json!({"host": "example.org", "ports": []}),
        json!({"host": "", "ports": [443]}),
        json!({"host": "example.org", "ports": [0]}),
        json!({"host": "example.org", "ports": [-1]}),
        json!({"host": "example.org", "ports": [65536]}),
        json!({"host": "example.org", "ports": [443.5]}),
        json!({"host": "example.org", "ports": ["443"]}),
        json!({"host": "example.org", "ports": [true]}),
        json!({"host": "example.org", "ports": [null]}),
        json!({"host": "example.org", "ports": [443], "extra": true}),
    ] {
        fs::write(
            &profile,
            json!({
                "schema": 1, "profile": {"name": "base", "network_allowlist": [rule]},
                "image": {"ref": "wrix:test", "source": "/missing", "source_kind": kind},
                "agent": {"kind": "direct"}
            })
            .to_string(),
        )
        .unwrap();
        for args in [
            vec!["run"],
            vec!["spawn", "--spawn-config", spawn.to_str().unwrap()],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
                .arg("--profile-config")
                .arg(&profile)
                .args(args)
                .current_dir(&workspace)
                .env("HOME", root.path().join("home"))
                .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
                .env("PATH", root.path().join("no-executables"))
                .output()
                .unwrap();
            assert!(!output.status.success(), "{rule}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("invalid ProfileConfig schema"), "{stderr}");
            assert!(!root.path().join("home").exists());
            assert!(!root.path().join("runtime").exists());
            assert_eq!(fs::read_dir(&workspace).unwrap().count(), 0);
        }
    }
}
