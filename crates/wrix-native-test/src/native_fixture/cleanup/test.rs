use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Fault {
    Unmount,
    Stop,
    Diagnostics,
    Removal,
}

struct Effects {
    host: Vec<mount::Entry>,
    namespace: Vec<mount::Entry>,
    calls: Vec<(Vec<OsString>, Duration)>,
    fault: Option<Fault>,
}
impl Effects {
    fn empty() -> Self {
        Self {
            host: vec![],
            namespace: vec![],
            calls: vec![],
            fault: None,
        }
    }
}
fn entry(target: PathBuf, kind: &str) -> mount::Entry {
    mount::Entry {
        target,
        kind: kind.as_bytes().to_vec(),
        source: format!("wrix-nix-{}", "a".repeat(32)).into_bytes(),
    }
}
fn view(root: &Path) -> PathBuf {
    root.join("home/.local/state/wrix/control/nix")
        .join("b".repeat(32))
        .join("x86_64-linux/executions")
        .join("c".repeat(64))
        .join("store")
}
fn table(entries: &[mount::Entry]) -> Vec<u8> {
    entries
        .iter()
        .flat_map(|entry| {
            format!(
                "90 2 0:99 / {} ro,nosuid,nodev - {} {} ro\n",
                entry
                    .target
                    .display()
                    .to_string()
                    .replace('\\', "\\134")
                    .replace(' ', "\\040"),
                String::from_utf8(entry.kind.clone()).unwrap(),
                String::from_utf8(entry.source.clone()).unwrap()
            )
            .into_bytes()
        })
        .collect()
}
impl Runtime for Effects {
    fn host_mounts(&mut self) -> Result<Vec<mount::Entry>, Error> {
        Ok(mount::parse(&table(&self.host))?)
    }
    fn execute(
        &mut self,
        environment: &command::Environment,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let actual = environment.command("ignored");
        assert!(actual.get_envs().any(|(key, _)| key == "HOME"));
        self.calls.push((args.to_vec(), timeout));
        let has = |value: &str| args.iter().any(|arg| arg == value);
        if has("ps") && self.fault == Some(Fault::Diagnostics) {
            return Err(Error::Policy("diagnostic deadline"));
        }
        if Path::new(&args[0]).file_name() == Some(OsStr::new("fusermount3")) {
            if self.fault == Some(Fault::Unmount) {
                return Err(Error::Policy("Device or resource busy"));
            }
            assert_eq!(&args[1..3], ["-u", "--"]);
            self.host
                .retain(|entry| entry.target != Path::new(&args[3]));
        } else if has("unshare")
            && args
                .iter()
                .any(|arg| Path::new(arg).file_name() == Some(OsStr::new("fusermount3")))
        {
            self.namespace.retain(|entry| {
                Some(entry.target.as_os_str()) != args.last().map(OsString::as_os_str)
            });
        } else if args.last() == Some(&OsString::from("/proc/self/mountinfo")) {
            return Ok(table(&self.namespace));
        } else if has("stop") && self.fault == Some(Fault::Stop) {
            return Err(Error::Policy("fixture runtime unavailable"));
        } else if has("ps") {
            return Ok(format!("{}\n", "d".repeat(64)).into_bytes());
        } else if has("inspect") {
            return Ok(b"{\"Running\":true}".to_vec());
        } else if has("logs") {
            return Ok(b"native bootstrap phase\n".to_vec());
        }
        Ok(vec![])
    }
    fn remove_tree(&mut self, path: &Path) -> Result<(), Error> {
        if self.fault == Some(Fault::Removal) {
            return Err(
                io::Error::new(io::ErrorKind::PermissionDenied, "mapped layer owner").into(),
            );
        }
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
fn subject(storage: bool) -> (tempfile::TempDir, Subject) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(root::MARKER), "1\n").unwrap();
    if storage {
        fs::create_dir_all(root.path().join("home/.local/share/containers/storage")).unwrap();
    }
    let subject = Subject::new(
        root::Root::parse(root.path(), root::Origin::Discovered).unwrap(),
        None,
    )
    .unwrap();
    (root, subject)
}
fn has(call: &(Vec<OsString>, Duration), word: &str) -> bool {
    call.0.iter().any(|arg| arg == word)
}

#[test]
fn teardown_is_scoped_ordered_and_checks_both_mount_namespaces() {
    let (root, subject) = subject(true);
    let selected = view(root.path());
    let peer = PathBuf::from("/peer/export");
    let mut effects = Effects::empty();
    effects.host = vec![
        entry(selected.clone(), "fuse.sshfs"),
        entry(peer.clone(), "fuse.sshfs"),
    ];
    effects.namespace = vec![entry(selected, "fuse.sshfs")];
    subject.finish(&mut effects, false).unwrap();
    assert!(has(&effects.calls[0], "stop"));
    assert!(has(effects.calls.last().unwrap(), "migrate"));
    for value in ["rm", "rmi", "chown", "chmod"] {
        assert!(effects.calls.iter().any(|call| has(call, value)));
    }
    let unmount = effects
        .calls
        .iter()
        .position(|call| {
            call.0
                .iter()
                .any(|arg| Path::new(arg).file_name() == Some(OsStr::new("fusermount3")))
        })
        .unwrap();
    assert!(
        unmount
            < effects
                .calls
                .iter()
                .position(|call| has(call, "rm"))
                .unwrap()
    );
    assert!(subject.root.stopped().unwrap());
    assert_eq!(effects.host[0].target, peer);
    assert_eq!(effects.namespace, []);
    assert!(
        !effects
            .calls
            .iter()
            .any(|call| has(call, "--force") || has(call, "-z"))
    );
    assert_eq!(subject.podman[3], subject.storage.as_os_str());
}
#[test]
fn failed_unmount_cannot_remove_container_ownership_or_mark_teardown() {
    let (root, subject) = subject(true);
    let mut effects = Effects::empty();
    effects.fault = Some(Fault::Unmount);
    effects.host.push(entry(view(root.path()), "fuse.sshfs"));
    assert!(subject.finish(&mut effects, false).is_err());
    assert!(
        !effects
            .calls
            .iter()
            .any(|call| has(call, "rm") || has(call, "migrate"))
    );
    assert!(!subject.root.stopped().unwrap());
    assert!(root.path().exists());
}
#[test]
fn administrative_bind_mounts_do_not_block_pause_namespace_release() {
    let (root, subject) = subject(true);
    let mut effects = Effects::empty();
    effects.namespace = vec![
        entry(subject.storage.clone(), "ext4"),
        entry(subject.storage.join("overlay"), "ext4"),
        entry(root.path().join("runtime/containers"), "tmpfs"),
    ];
    subject.finish(&mut effects, false).unwrap();
    assert!(has(effects.calls.last().unwrap(), "migrate"));
}
#[test]
fn non_administrative_mounts_block_owner_changes_and_retirement() {
    for (relative, kind) in [
        ("unknown", "ext4"),
        ("home/.local/share/containers/storage/overlay", "overlay"),
        (
            "home/.local/share/containers/storage/overlay",
            "fuse.overlayfs",
        ),
        (
            "home/.local/share/containers/storage/overlay/layer/merged",
            "overlay",
        ),
    ] {
        let (root, subject) = subject(true);
        let mut effects = Effects::empty();
        effects
            .namespace
            .push(entry(root.path().join(relative), kind));
        assert!(subject.finish(&mut effects, false).is_err());
        assert!(
            !effects
                .calls
                .iter()
                .any(|call| has(call, "chown") || has(call, "migrate"))
        );
        assert!(!subject.root.stopped().unwrap());
    }
}
#[test]
fn remaining_host_mount_prevents_receipt_and_file_deletion() {
    let (root, subject) = subject(false);
    let mut effects = Effects::empty();
    effects
        .host
        .push(entry(root.path().join("unknown"), "ext4"));
    assert!(matches!(
        subject.finish(&mut effects, false),
        Err(Error::Mounted(_))
    ));
    assert!(!subject.root.stopped().unwrap());
    assert!(subject.remove_files(&mut effects).is_err());
    assert!(root.path().exists());
}
#[test]
fn diagnostics_are_bounded_private_and_captured_before_stop() {
    let (root, subject) = subject(true);
    let mut effects = Effects::empty();
    subject.finish(&mut effects, true).unwrap();
    assert!(has(&effects.calls[0], "ps"));
    assert!(
        effects.calls[..3]
            .iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(3))
    );
    let mut contents = fs::read_dir(root.path().join("native-cleanup-diagnostics"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert_eq!(entry.metadata().unwrap().mode() & 0o777, 0o600);
            fs::read(entry.path()).unwrap()
        })
        .collect::<Vec<_>>();
    contents.sort();
    let mut expected = vec![
        b"{\"Running\":true}".to_vec(),
        b"native bootstrap phase\n".to_vec(),
    ];
    expected.sort();
    assert_eq!(contents, expected);
}
#[test]
fn unavailable_diagnostics_do_not_suppress_teardown_or_receipt() {
    let (_root, subject) = subject(true);
    let mut effects = Effects::empty();
    effects.fault = Some(Fault::Diagnostics);
    subject.finish(&mut effects, true).unwrap();
    assert!(subject.root.stopped().unwrap());
    assert!(effects.calls.iter().any(|call| has(call, "stop")));
}
#[test]
fn stopped_retry_never_restarts_podman_and_partial_removal_preserves_evidence() {
    let (root, subject) = subject(true);
    let mut effects = Effects::empty();
    subject.finish(&mut effects, false).unwrap();
    effects.fault = Some(Fault::Removal);
    assert!(subject.remove_files(&mut effects).is_err());
    assert!(root.path().join(root::MARKER).is_file());
    assert!(root.path().join("runtime").is_dir());
    assert!(subject.root.stopped().unwrap());
    effects.fault = None;
    effects.calls.clear();
    subject.finish(&mut effects, false).unwrap();
    assert_eq!(effects.calls, []);
    subject.remove_files(&mut effects).unwrap();
    assert!(!root.path().exists());
}
#[test]
fn stopped_fixture_removes_mode_zero_runtime_without_following_peer_symlinks() {
    let (root, subject) = subject(false);
    let peer = tempfile::tempdir().unwrap();
    let directory = root.path().join("runtime/systemd/inaccessible/dir");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o0)).unwrap();
    fs::set_permissions(
        directory.parent().unwrap(),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    symlink(peer.path(), root.path().join("runtime/peer")).unwrap();
    fs::set_permissions(peer.path(), fs::Permissions::from_mode(0o500)).unwrap();
    subject.root.record_stopped().unwrap();
    subject.remove_files(&mut Effects::empty()).unwrap();
    assert!(!root.path().exists());
    assert_eq!(peer.path().metadata().unwrap().mode() & 0o777, 0o500);
    fs::set_permissions(peer.path(), fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn old_partial_cleanup_recreates_only_private_runtime_before_podman() {
    let (root, subject) = subject(true);
    assert!(!root.path().join("runtime").exists());
    subject.finish(&mut Effects::empty(), false).unwrap();
    assert_eq!(
        root.path().join("runtime").metadata().unwrap().mode() & 0o777,
        0o700
    );
}
#[test]
fn runtime_command_failure_preserves_files_and_unreceipted_state() {
    let (root, subject) = subject(true);
    let mut effects = Effects::empty();
    effects.fault = Some(Fault::Stop);
    assert!(subject.finish(&mut effects, false).is_err());
    assert!(!subject.root.stopped().unwrap());
    assert!(root.path().exists());
    assert!(subject.remove_files(&mut effects).is_err());
}
#[test]
fn registered_roots_can_recover_removed_markers_without_trusting_other_paths() {
    let (root, _subject) = subject(false);
    fs::remove_file(root.path().join(root::MARKER)).unwrap();
    assert!(root::Root::parse(root.path(), root::Origin::Discovered).is_err());
    let registered = Subject::new(
        root::Root::parse(root.path(), root::Origin::Registered).unwrap(),
        None,
    )
    .unwrap();
    registered.finish(&mut Effects::empty(), false).unwrap();
    registered.remove_files(&mut Effects::empty()).unwrap();
    assert!(!root.path().exists());
}
#[test]
fn private_readonly_to_peers_0755_root_is_supported_without_mode_repair() {
    let (root, _subject) = subject(false);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let subject = Subject::new(
        root::Root::parse(root.path(), root::Origin::Discovered).unwrap(),
        None,
    )
    .unwrap();
    subject.finish(&mut Effects::empty(), false).unwrap();
    assert_eq!(root.path().metadata().unwrap().mode() & 0o777, 0o755);
}
