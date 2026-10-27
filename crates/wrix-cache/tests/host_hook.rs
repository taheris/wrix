//! Real hook -> owner selection -> publisher -> native database, without a host installation.
#![cfg(test)]
#[path = "../../wrix-cli/tests/workspace/isolation.rs"]
mod isolation;

use fs2::FileExt;
use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use wrix_cache::config::Settings;
use wrix_core::path::WorkspaceHash;
use wrix_state::{context::Context, host::Host, namespace, store::Store};

fn assign_owner(root: &Path, uid: rustix::process::Uid, gid: rustix::process::Gid) {
    if uid != rustix::process::geteuid() {
        assert!(
            Command::new("chown")
                .args(["-R", "-h", &format!("{}:{}", uid.as_raw(), gid.as_raw())])
                .arg(root)
                .status()
                .unwrap()
                .success()
        );
    }
}

const DRV: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-project.drv";

fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join(name))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap()
}

#[test]
#[ignore = "child process fixture; invoked explicitly across the host boundary"]
fn prepare_native_scope() {
    let checkout = PathBuf::from(std::env::var_os("WRIX_TEST_CHECKOUT").unwrap());
    let host = Host::discover().unwrap();
    Store::initialize(&host).unwrap();
    Store::acknowledge(&host, 1).unwrap();
    let context = Context::inspect(&host, &checkout).unwrap();
    context.authorize_effects(&host).unwrap();
    let state = host.roots().workspace_state(context.key());
    fs::create_dir_all(&state).unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!({"schema_version":1,"workspace_hash":context.key().as_str(),"roots":[{"name":"project","installable":".#project","drv_path":DRV,"out_paths":[]}]})).unwrap();
    let path = state.join("publish-roots.json");
    namespace::write_at(&path, &manifest).unwrap();
    fs::write(
        std::env::var_os("WRIX_TEST_KEY_FILE").unwrap(),
        context.key().as_str(),
    )
    .unwrap();
}

#[test]
#[ignore = "child process verifier; invoked explicitly across the host boundary"]
fn verify_native_queue() {
    let key = WorkspaceHash::parse(
        fs::read_to_string(std::env::var_os("WRIX_TEST_KEY_FILE").unwrap())
            .unwrap()
            .trim(),
    )
    .unwrap();
    let host = Host::discover().unwrap();
    let namespace::Listing::Host(rows) =
        namespace::pending_at(&host.roots().workspace_state(&key).join("pending")).unwrap()
    else {
        panic!("expected host authority");
    };
    assert_eq!(rows.len(), 1);
    let value: serde_json::Value = serde_json::from_slice(&rows[0].1).unwrap();
    assert_eq!(value["drv_path"], DRV);
}

#[test]
fn host_hook_uses_database_only_metadata_and_queues_under_contention() {
    let hook = Path::new(env!("CARGO_BIN_EXE_wrix-cache-hook"));
    let publisher = Path::new(env!("CARGO_BIN_EXE_wrix-cache-publish"));
    let setup = std::env::current_exe().unwrap();
    let (root, isolated) = isolation::root(hook);
    let inside = |relative: &str| {
        if isolated {
            Path::new("/").join(relative)
        } else {
            root.path().join(relative)
        }
    };
    fs::create_dir(root.path().join("checkout")).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(root.path().join("checkout"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success()
    );
    if isolated {
        isolation::install_executable(root.path(), &executable("git"), Path::new("/bin/git"));
        isolation::install_executable(root.path(), &setup, Path::new("/bin/setup"));
        isolation::install_executable(root.path(), publisher, Path::new("/bin/publisher"));
    }
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let nix_store = bin.join("nix-store");
    let bash = if isolated {
        isolation::install_executable(root.path(), &executable("bash"), Path::new("/bin/bash"));
        PathBuf::from("/bin/bash")
    } else {
        executable("bash")
    };
    fs::write(&nix_store, r#"#!/bin/bash
set -euo pipefail
[[ $# == 8 && "$1" == --option && "$2" == substitute && "$3" == false && "$4" == --add-root && "$6" == --indirect && "$7" == --realise ]]
[[ "$8" == /nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-output ]]
ln -s "$8" "$5"
printf '%s\n' "$8"
"#.replacen("#!/bin/bash", &format!("#!{}", bash.display()), 1)).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&nix_store, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let owner = if rustix::process::geteuid().is_root() {
        (
            rustix::process::Uid::from_raw(12345),
            rustix::process::Gid::from_raw(12345),
        )
    } else {
        (rustix::process::geteuid(), rustix::process::getegid())
    };
    assign_owner(root.path(), owner.0, owner.1);
    let setup_path = if isolated {
        Path::new("/bin/setup")
    } else {
        &setup
    };
    let output = isolation::command_as(
        root.path(),
        isolated,
        setup_path,
        &[
            "--exact",
            "prepare_native_scope",
            "--ignored",
            "--nocapture",
        ],
        Some(owner),
    )
    .env("WRIX_TEST_CHECKOUT", inside("checkout"))
    .env("WRIX_TEST_KEY_FILE", inside("key"))
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let key =
        WorkspaceHash::parse(fs::read_to_string(root.path().join("key")).unwrap().trim()).unwrap();
    let state_base = if cfg!(target_os = "macos") {
        "home/Library/Application Support/wrix"
    } else {
        "home/.local/state/wrix"
    };
    let cache_base = if cfg!(target_os = "macos") {
        "home/Library/Caches/wrix"
    } else {
        "home/.cache/wrix"
    };
    let state_relative = format!("{state_base}/workspaces/{key}");
    let cache_relative = format!("{cache_base}/workspaces/{key}/binary-cache");
    fs::create_dir_all(root.path().join(&cache_relative)).unwrap();
    let native = root.path().join(&state_relative);
    let keys = native.join("keys");
    fs::create_dir(&keys).unwrap();
    let output = Command::new("nix-store")
        .args(["--generate-binary-cache-key", "fixture"])
        .arg(keys.join("cache.secret"))
        .arg(keys.join("cache.pub"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let secret_before = fs::read(keys.join("cache.secret")).unwrap();
    assert!(!native.join("publish-roots.json").exists());
    let settings = Settings {
        lock_timeout_ms: 1,
        ..Settings::default()
    };
    fs::write(
        root.path().join("settings.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(native.join("cache.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let state = inside(&state_relative);
    let cache = inside(&cache_relative);
    let manifest = state.join("publish-roots.json");
    let helper = if isolated {
        Path::new("/bin/publisher")
    } else {
        publisher
    };
    let uid = owner.0.as_raw().to_string();
    let gid = owner.1.as_raw().to_string();
    assign_owner(root.path(), owner.0, owner.1);
    let config = inside("settings.json");
    let output = isolation::command(
        root.path(),
        isolated,
        if isolated {
            Path::new("/bin/wrix")
        } else {
            hook
        },
        &[
            "--workspace-hash",
            key.as_str(),
            "--owner-uid",
            &uid,
            "--owner-gid",
            &gid,
            "--state-root",
            state.to_str().unwrap(),
            "--cache-root",
            cache.to_str().unwrap(),
            "--manifest",
            manifest.to_str().unwrap(),
            "--publisher-helper",
            helper.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
        ],
    )
    .env(
        "PATH",
        if isolated {
            std::ffi::OsString::from("/bin:/usr/bin")
        } else {
            let mut paths = vec![bin];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            std::env::join_paths(paths).unwrap()
        },
    )
    .env("DRV_PATH", DRV)
    .env(
        "OUT_PATHS",
        "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-output",
    )
    .output()
    .unwrap();
    fs2::FileExt::unlock(&lock).unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for name in [
        "services.json",
        "cache-status.json",
        "publish-roots.json",
        "pending",
        "gcroots",
    ] {
        assert!(!native.join(name).exists(), "{name}");
    }
    let output = isolation::command_as(
        root.path(),
        isolated,
        setup_path,
        &["--exact", "verify_native_queue", "--ignored", "--nocapture"],
        Some(owner),
    )
    .env("WRIX_TEST_KEY_FILE", inside("key"))
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(keys.join("cache.secret")).unwrap(), secret_before);
    assert!(!native.join("publish-roots.json").exists());
}
