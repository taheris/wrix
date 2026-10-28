use super::{Fixture, legacy_services};
use crate::common::{self, TestResult, set_mode};
#[path = "../common/celler.rs"]
mod celler;
#[path = "../common/controller.rs"]
#[expect(
    dead_code,
    reason = "consolidation binds the real controller to an existing admitted host root"
)]
mod controller;
#[path = "../common/service_binding.rs"]
#[expect(
    dead_code,
    reason = "fresh shared startup provisions its own issuer through production lifecycle"
)]
mod service_binding;

struct Services {
    controller: controller::Server,
    _celler: celler::Server,
    _sql: std::os::unix::net::UnixListener,
}
impl Services {
    fn new(fixture: &Fixture) -> TestResult<Self> {
        let root = fixture.root.path();
        let bin = root.join("bin");
        service_binding::create(root, &bin, "nix-descriptor")?;
        for name in ["podman", "container", "skopeo"] {
            fs::write(
                bin.join(name),
                concat!(
                    include_str!("../fixtures/shared-readiness.sh"),
                    "\n",
                    include_str!("../fixtures/service-runtime.sh")
                ),
            )?;
            set_mode(&bin.join(name), 0o700)?;
        }
        fs::write(root.join("image"), "installed")?;
        let data = fixture.state().join("service-data/dolt");
        wrix_core::private_directory::prepare_with_parents(&data)?;
        wrix_core::private_directory::prepare_with_parents(&data.join("run"))?;
        let sql = std::os::unix::net::UnixListener::bind(data.join("run/dolt.sock"))?;
        let controller = controller::Server::start_at(root, &fixture.state())?;
        #[cfg(target_os = "linux")]
        let controller = controller::Server::supervise(controller, root, &fixture.state())?;
        Ok(Self {
            controller,
            _celler: celler::Server::watch(root, None),
            _sql: sql,
        })
    }

    fn configure(&self, fixture: &Fixture, command: &mut Command) -> TestResult {
        let root = fixture.root.path();
        let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("missing PATH")?),
        ))?;
        command
            .env("PATH", path)
            .env(
                "XDG_STATE_HOME",
                fixture.state().parent().ok_or("missing state parent")?,
            )
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("WRIX_SERVICE_PACKAGE_BINDING", root.join("binding.json"))
            .env("WRIX_CONTAINER_RUNTIME", root.join("bin/podman"))
            .env("WRIX_TEST_CALLS", root.join("calls"))
            .env("WRIX_TEST_JQ_BIN", common::command_path("jq")?)
            .env("WRIX_TEST_IMAGE", root.join("image"))
            .env("WRIX_TEST_RUNNING", root.join("running"))
            .env("WRIX_TEST_RUN_ARGS", root.join("run.argv"))
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)));
        self.controller.configure(command)
    }
}

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
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

fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if !path.exists() {
        return files;
    }
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            files.extend(snapshot(&entry.path()));
        } else if kind.is_symlink() {
            files.insert(
                entry.path(),
                fs::read_link(entry.path())
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            );
        } else if kind.is_file() {
            files.insert(entry.path(), fs::read(entry.path()).unwrap());
        } else {
            files.insert(entry.path(), Vec::new());
        }
    }
    files
}

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
}

fn run(fixture: &Fixture, services: &Services, checkout: &Path, args: &[&str]) -> Output {
    let mut command = fixture.application_command(checkout, args);
    services.configure(fixture, &mut command).unwrap();
    let output = command
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
    let services = Services::new(&fixture).unwrap();
    let key = WorkspaceHash::from_path(&checkout);
    let native = fixture.state().join("workspaces").join(key.as_str());
    run(
        &fixture,
        &services,
        &checkout,
        &["service", "start", "--no-cache"],
    );
    keys(&native, &key);
    let keys_before = snapshot(&native.join("keys"));
    assert_eq!(keys_before.len(), 2);
    run(
        &fixture,
        &services,
        &checkout,
        &["service", "cache", "publish"],
    );
    let before = snapshot(&fixture.state());
    let status = run(
        &fixture,
        &services,
        &checkout,
        &["service", "cache", "status"],
    );
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
    let services = Services::new(&fixture).unwrap();
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
    run(
        &fixture,
        &services,
        &checkout,
        &["service", "start", "--no-cache"],
    );
    keys(&native, &key);
    run(
        &fixture,
        &services,
        &checkout,
        &["service", "cache", "publish"],
    );
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
    let status = run(
        &fixture,
        &services,
        &checkout,
        &["service", "cache", "status"],
    );
    assert!(String::from_utf8_lossy(&status.stdout).contains("pending records: 0"));
}
