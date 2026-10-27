//! A private, bounded checkpoint for deliberate native-test interruption.
use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

pub(super) const NONCE: &str = "WRIX_TEST_POOL_INTERRUPT_NONCE";
const READY: &str = ".wrix-native-test-ready.json";
const PROGRESS: &str = ".wrix-native-test-progress.json";

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Phase {
    Controller,
    WorkerImage,
    FirstWorker,
    SecondWorker,
    WorkerEvaluations,
    WorkersReady,
}

pub(super) fn progress(workspace: &Path, phase: Phase) {
    if let Some(nonce) = std::env::var_os(NONCE) {
        publish(workspace, nonce.to_str().unwrap(), phase);
    }
}

pub(super) fn pause(workspace: &Path) {
    if let Some(nonce) = std::env::var_os(NONCE) {
        checkpoint(
            workspace,
            nonce.to_str().expect("interruption nonce must be ASCII"),
        );
    }
}

fn publish(workspace: &Path, nonce: &str, phase: Phase) {
    assert!(nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let root = workspace.parent().unwrap();
    let identity = fs::metadata(root).unwrap();
    let record = serde_json::json!({
        "version": 1, "phase": phase, "nonce": nonce,
        "device": identity.dev(), "inode": identity.ino(),
    });
    let destination = root.join(match phase {
        Phase::WorkersReady => READY,
        _ => PROGRESS,
    });
    let temporary = destination.with_extension("pending");
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .unwrap()
    };
    file.write_all(&serde_json::to_vec(&record).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(temporary, destination).unwrap();
    fs::File::open(root).unwrap().sync_all().unwrap();
}

fn checkpoint(workspace: &Path, nonce: &str) {
    publish(workspace, nonce, Phase::WorkersReady);
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("native interruption checkpoint expired without supervisor termination");
}

#[test]
fn progress_is_private_atomic_and_bound_to_the_fixture() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("checkout");
    fs::create_dir(&workspace).unwrap();
    let nonce = "a".repeat(64);
    publish(&workspace, &nonce, Phase::FirstWorker);
    publish(&workspace, &nonce, Phase::SecondWorker);
    let path = root.path().join(PROGRESS);
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    let identity = fs::metadata(root.path()).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        record,
        serde_json::json!({"version": 1, "phase": "second-worker", "nonce": nonce, "device": identity.dev(), "inode": identity.ino()})
    );
    assert!(!root.path().join(READY).exists());
    assert!(
        !root
            .path()
            .join(".wrix-native-test-progress.pending")
            .exists()
    );
}

#[test]
fn invalid_interruption_nonce_cannot_publish_readiness() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("checkout");
    fs::create_dir(&workspace).unwrap();
    assert!(std::panic::catch_unwind(|| checkpoint(&workspace, "not-a-nonce")).is_err());
    assert!(!root.path().join(READY).exists());
}

#[test]
#[cfg(target_os = "linux")]
fn checkpoint_publishes_root_bound_readiness_and_holds_until_interrupted() {
    use std::{os::unix::process::CommandExt, process::Command};
    const CHILD: &str = "WRIX_CHECKPOINT_TEST_ROOT";
    let nonce = "a".repeat(64);
    if let Some(root) = std::env::var_os(CHILD) {
        checkpoint(&Path::new(&root).join("checkout"), &nonce);
        return;
    }
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("checkout")).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "nix_store::admission::interrupt::checkpoint_publishes_root_bound_readiness_and_holds_until_interrupted", "--nocapture"])
        .env(CHILD, root.path()).process_group(0);
    let mut child = super::Running(command.spawn().unwrap());
    super::wait_for(&root.path().join(READY));
    assert!(child.0.try_wait().unwrap().is_none());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(READY)).unwrap()).unwrap();
    let identity = fs::metadata(root.path()).unwrap();
    assert_eq!(
        record,
        serde_json::json!({"version":1, "phase":"workers-ready", "nonce":nonce, "device":identity.dev(), "inode":identity.ino()})
    );
    assert!(!root.path().join(".wrix-native-test-ready.pending").exists());
    drop(child);
}
