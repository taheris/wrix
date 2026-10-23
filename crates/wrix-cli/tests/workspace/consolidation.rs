use super::{Fixture, legacy_services, snapshot};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use wrix_core::path::WorkspaceHash;

const DOCUMENTS: [&str; 5] = [
    "services.json",
    "cache-status.json",
    "publish-roots.json",
    "pending",
    "gcroots",
];

fn prepare(fixture: &Fixture, checkout: &Path) {
    fs::write(
        fixture.root.path().join("checkout/wrix.toml"),
        "[cache.publish]\npackages = false\nchecks = false\ndev_shell = false\n",
    )
    .unwrap();
    assert!(
        fixture
            .git(checkout, &["add", "wrix.toml"])
            .status
            .success()
    );
    let runtime = fixture.root.path().join("podman");
    let bash = fixture.install("bash");
    fs::write(&runtime, format!("#!{}\nset -euo pipefail\nif [[ \"$#\" == 5 && \"$5\" == --type=container && \"$1\" == inspect && \"$2\" == --format && \"$3\" == '{{{{.State.Running}}}}' && \"$4\" == checkout-service ]]; then printf 'Error: no such container\\n' >&2; exit 125; fi\nprintf 'unexpected runtime arguments: %s\\n' \"$*\" >&2\nexit 64\n", bash.display())).unwrap();
    fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
}

fn run(fixture: &Fixture, checkout: &Path, args: &[&str]) -> Output {
    let output = fixture
        .application_command(checkout, args)
        .env("WRIX_CONTAINER_RUNTIME", fixture.inside("podman"))
        .env("WRIX_UPSTREAM_SUBSTITUTERS", "")
        .env(
            "WRIX_SYSTEM",
            format!(
                "{}-{}",
                std::env::consts::ARCH,
                if cfg!(target_os = "macos") {
                    "darwin"
                } else {
                    "linux"
                }
            ),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn keys(native: &Path, key: &WorkspaceHash) {
    let keys = native.join("keys");
    fs::create_dir_all(&keys).unwrap();
    let output = Command::new("nix-store")
        .args([
            "--generate-binary-cache-key",
            &format!("wrix-cache-{}", key.as_str()),
        ])
        .arg(keys.join("cache.secret"))
        .arg(keys.join("cache.pub"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_service_and_cache_commands_create_database_records_without_metadata_exports() {
    let fixture = Fixture::new();
    fixture.initialize();
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    let checkout = fixture.repository("checkout");
    prepare(&fixture, &checkout);
    let key = WorkspaceHash::from_path(&checkout);
    let native = fixture.state().join("workspaces").join(key.as_str());
    run(&fixture, &checkout, &["service", "start", "--no-cache"]);
    keys(&native, &key);
    let keys_before = snapshot(&native.join("keys"));
    assert_eq!(keys_before.len(), 2);
    run(&fixture, &checkout, &["service", "cache", "publish"]);
    let before = snapshot(&fixture.state());
    let status = run(&fixture, &checkout, &["service", "cache", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("publication roots: 0"));
    assert_eq!(snapshot(&fixture.state()), before);
    assert_eq!(snapshot(&native.join("keys")), keys_before);
    for name in DOCUMENTS {
        assert!(!native.join(name).exists(), "{name}");
    }
    for name in [
        "service.lock",
        "cache.lock",
        "keys/cache.secret",
        "keys/cache.pub",
    ] {
        assert!(native.join(name).is_file(), "{name}");
    }
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    for owner in ["services", "cache-status", "publish-roots"] {
        let bytes: Vec<u8> = db
            .query_row(
                "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
                [key.as_str(), owner],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("missing {owner} metadata: {error}"));
        assert!(
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .unwrap()
                .is_object()
        );
    }
}

#[test]
fn host_mutations_preserve_imported_files_while_retiring_database_pending_and_closures() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    prepare(&fixture, &checkout);
    let key = WorkspaceHash::from_path(&checkout);
    let native = legacy_services(&fixture, &key, "checkout-service");
    fs::create_dir(native.join("pending")).unwrap();
    fs::create_dir(native.join("gcroots")).unwrap();
    fs::write(
        native.join("cache-status.json"),
        r#"{"dirty":true,"last_publish":"legacy","last_prune":null,"last_error":null}"#,
    )
    .unwrap();
    fs::write(native.join("publish-roots.json"), r#"{"roots":[]}"#).unwrap();
    fs::write(
        native.join("pending/expired.json"),
        r#"{"created_at_epoch":1,"drv_path":"/nix/store/expired.drv","out_paths":[]}"#,
    )
    .unwrap();
    fs::write(native.join("gcroots/obsolete"), "/nix/store/obsolete\n").unwrap();
    let legacy = snapshot(&native);
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    run(&fixture, &checkout, &["service", "start", "--no-cache"]);
    keys(&native, &key);
    run(&fixture, &checkout, &["service", "cache", "publish"]);
    for (path, bytes) in legacy {
        assert_eq!(fs::read(&path).unwrap(), bytes, "{}", path.display());
    }
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    let count: i64 = db.query_row(
        "SELECT count(*) FROM namespace_metadata WHERE native_key = ?1 AND (owner LIKE 'pending/%' OR owner LIKE 'cache-closure/%')",
        [key.as_str()],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
    let status = run(&fixture, &checkout, &["service", "cache", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("pending records: 0"));
}
