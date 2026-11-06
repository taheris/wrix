#![cfg(test)]

mod common;

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

fn install(source: &Path, root: &Path, mode: &str, success: bool) {
    let output = Command::new("bash")
        .arg(common::repo_file("lib/sandbox/install-known-hosts.sh"))
        .args([source.as_os_str(), root.as_os_str(), mode.as_ref()])
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct RestoreMode(PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755))
            .expect("restore owned fixture permissions");
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn unchanged_fields(info: &fs::Metadata) -> Identity {
    // Reads may change atime, but never identity, ownership or write timestamps.
    Identity {
        device: info.dev(),
        inode: info.ino(),
        mode: info.mode(),
        links: info.nlink(),
        uid: info.uid(),
        gid: info.gid(),
        size: info.size(),
        modified: (info.mtime(), info.mtime_nsec()),
        changed: (info.ctime(), info.ctime_nsec()),
    }
}

// All three original Python cases, including each build/runtime subcase.
#[test]
fn materializes_store_directory_in_build_and_runtime_modes() {
    for mode in ["build", "runtime"] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("image with spaces");
        fs::create_dir_all(root.join("etc")).unwrap();
        let store = parent.path().join("store ssh");
        fs::create_dir(&store).unwrap();
        let config = store.join("ssh_config");
        fs::write(&config, "Host *\n  StrictHostKeyChecking yes\n").unwrap();
        let external = parent.path().join("external known hosts");
        fs::write(&external, "external data\n").unwrap();
        symlink(&external, store.join("ssh_known_hosts")).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o555)).unwrap();
        let _restore = RestoreMode(store.clone());
        symlink(&store, root.join("etc/ssh")).unwrap();
        let source = parent.path().join("pinned hosts");
        fs::write(&source, "github.com ssh-ed25519 pinned\n").unwrap();
        let before = unchanged_fields(&fs::metadata(&external).unwrap());
        install(&source, &root, mode, true);
        assert_eq!(
            unchanged_fields(&fs::metadata(&external).unwrap()),
            before,
            "{mode}"
        );
        assert!(
            !fs::symlink_metadata(root.join("etc/ssh"))
                .unwrap()
                .is_symlink()
        );
        assert!(
            !fs::symlink_metadata(root.join("etc/ssh/ssh_known_hosts"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            fs::read(root.join("etc/ssh/ssh_known_hosts")).unwrap(),
            fs::read(&source).unwrap()
        );
        assert_eq!(
            fs::read(root.join("etc/ssh/ssh_config")).unwrap(),
            fs::read(&config).unwrap()
        );
        assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, 0o444);
        assert_eq!(fs::metadata(&store).unwrap().mode() & 0o777, 0o555);
        assert!(
            fs::symlink_metadata(store.join("ssh_known_hosts"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&external).unwrap(), "external data\n");
        install(&source, &root, mode, true);
        assert_eq!(
            fs::read(root.join("etc/ssh/ssh_known_hosts")).unwrap(),
            fs::read(&source).unwrap()
        );
    }
}

#[test]
fn rejects_ancestor_symlinks_without_touching_external_targets() {
    for mode in ["build", "runtime"] {
        for target in ["etc", "etc/wrix", "etc/wrix/known_hosts_dir"] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("image");
            let alias = root.join(target);
            fs::create_dir_all(alias.parent().unwrap()).unwrap();
            let outside = parent.path().join("outside");
            fs::create_dir(&outside).unwrap();
            symlink(&outside, alias).unwrap();
            let source = parent.path().join("hosts");
            fs::write(&source, "pinned hosts\n").unwrap();
            install(&source, &root, mode, false);
            assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
        }
    }
}

#[test]
fn rejects_dangling_or_non_directory_ssh_targets() {
    for mode in ["build", "runtime"] {
        for file in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("image");
            fs::create_dir_all(root.join("etc")).unwrap();
            let outside = parent.path().join("target");
            if file {
                fs::write(&outside, "external data\n").unwrap();
            }
            symlink(&outside, root.join("etc/ssh")).unwrap();
            let source = parent.path().join("hosts");
            fs::write(&source, "pinned hosts\n").unwrap();
            install(&source, &root, mode, false);
            assert!(
                fs::symlink_metadata(root.join("etc/ssh"))
                    .unwrap()
                    .is_symlink()
            );
            if file {
                assert_eq!(fs::read_to_string(outside).unwrap(), "external data\n");
            } else {
                assert!(!outside.exists());
            }
        }
    }
}
