#[cfg(test)]
mod test {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    const INITIALIZER: &str = include_str!("../../../lib/sandbox/initialize-private-nix.sh");

    fn roots() -> Vec<String> {
        ["passwd", "group", "nix.conf"]
            .map(|name| format!("/nix/store/{}-{name}", "a".repeat(32)))
            .to_vec()
    }

    fn manifest() -> Vec<u8> {
        format!("{}\n", roots().join("\n")).into_bytes()
    }

    fn write(root: &Path, name: &str, data: &[u8]) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn initialize(root: &Path) -> Output {
        let temporary = tempfile::tempdir().unwrap();
        let script = temporary.path().join("initialize-private-nix.sh");
        fs::write(&script, INITIALIZER).unwrap();
        Command::new("bash").arg(script).arg(root).output().unwrap()
    }

    fn metadata(path: &Path) -> (u32, u32, u32, u64) {
        let output = Command::new("stat")
            .arg("--format=%u:%g:%a:%i")
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let fields = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<_> = fields.trim().split(':').collect();
        assert_eq!(fields.len(), 4);
        (
            fields[0].parse().unwrap(),
            fields[1].parse().unwrap(),
            u32::from_str_radix(fields[2], 8).unwrap(),
            fields[3].parse().unwrap(),
        )
    }

    fn stat_snapshot(info: &fs::Metadata) -> [i128; 13] {
        [
            info.mode().into(),
            info.ino().into(),
            info.dev().into(),
            info.nlink().into(),
            info.uid().into(),
            info.gid().into(),
            info.len().into(),
            info.atime().into(),
            info.atime_nsec().into(),
            info.mtime().into(),
            info.mtime_nsec().into(),
            info.ctime().into(),
            info.ctime_nsec().into(),
        ]
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn native_private_nix_initialization() {
        let output = Command::new("fakeroot")
            .arg(std::env::current_exe().unwrap())
            .args(["--ignored", "staged_", "--nocapture", "--test-threads=1"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::io::stdout().lock().write_all(&output.stdout).unwrap();
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("6 passed; 0 failed")
        );
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_preserves_existing_state_inodes_and_idempotence() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let manifest = manifest();
        let files = [
            ("etc/wrix/private-nix-version", b"1\n".as_slice()),
            ("etc/wrix/store-roots", manifest.as_slice()),
            ("nix/store/existing/content", b"existing store contents"),
            ("nix/var/nix/db/db.sqlite", b"existing database bytes"),
            ("nix/var/nix/db/db.sqlite-wal", b"existing WAL bytes"),
            ("nix/var/nix/gc.lock", b"existing lock bytes"),
            ("nix/var/nix/gcroots/docker/existing", b"existing GC root"),
            ("nix/var/log/nix/drvs/existing", b"existing build log"),
        ];
        for (name, bytes) in files {
            write(root, name, bytes);
        }
        for name in ["nix", "nix/var", "nix/var/nix"] {
            let path = root.join(name);
            let status = Command::new("chown")
                .arg("1000:1000")
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            fs::set_permissions(path, fs::Permissions::from_mode(0o777)).unwrap();
        }
        let inodes: Vec<_> = files
            .iter()
            .map(|(name, _)| fs::metadata(root.join(name)).unwrap().ino())
            .collect();
        for _ in 0..2 {
            let output = initialize(root);
            assert!(output.status.success(), "{output:?}");
            for ((name, bytes), inode) in files.iter().zip(&inodes) {
                assert_eq!(fs::read(root.join(name)).unwrap(), *bytes, "{name}");
                assert_eq!(
                    fs::metadata(root.join(name)).unwrap().ino(),
                    *inode,
                    "{name}"
                );
            }
            for name in ["nix", "nix/var", "nix/var/nix"] {
                let (uid, _, mode, _) = metadata(&root.join(name));
                assert_eq!((uid, mode), (0, 0o755), "{name}");
            }
            for name in [
                "nix/store",
                "nix/var/nix/db",
                "nix/var/nix/builds",
                "nix/var/nix/gc.lock",
            ] {
                assert_eq!(metadata(&root.join(name)).0, 1000, "{name}");
            }
            assert_eq!(metadata(&root.join("nix/var/nix/builds")).2, 0o755);
            let directory = root.join("nix/var/nix/gcroots/wrix-image");
            let (uid, _, mode, _) = metadata(&directory);
            assert_eq!((uid, mode), (0, 0o755));
            let mut actual: Vec<_> = directory
                .read_dir()
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            actual.sort();
            let mut expected: Vec<_> = roots()
                .iter()
                .map(|root| Path::new(root).file_name().unwrap().to_owned())
                .collect();
            expected.sort();
            assert_eq!(actual, expected);
            for target in roots() {
                let link = directory.join(Path::new(&target).file_name().unwrap());
                assert!(link.is_symlink());
                assert_eq!(fs::read_link(link).unwrap(), PathBuf::from(target));
            }
            assert!(!root.join("homeless-shelter").exists());
            assert!(!root.join("nix/var/nix/daemon-socket").exists());
        }
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_rejects_live_roots_without_replacing_them() {
        for root in ["/", "/nix", "/nix/var", "/nix/var/nix", "/nix/store"] {
            let path = Path::new(root);
            let before = match fs::symlink_metadata(path) {
                Ok(metadata) => Some(metadata.ino()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("{error}"),
            };
            let output = initialize(path);
            assert!(!output.status.success(), "{root}");
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .contains("requires a staged image root")
            );
            if let Some(inode) = before {
                assert_eq!(fs::symlink_metadata(path).unwrap().ino(), inode);
            } else {
                assert!(!path.exists());
                assert!(!path.is_symlink());
            }
        }
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_rejects_aliases_without_touching_external_targets() {
        for target in [
            "nix",
            "nix/store",
            "nix/var/nix/db",
            "nix/var/nix/gc.lock",
            "nix/var/nix/gcroots/wrix-image",
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("stage");
            write(&root, "etc/wrix/private-nix-version", b"1\n");
            let outside = temporary.path().join("outside");
            fs::create_dir(&outside).unwrap();
            let content = outside.join("content");
            fs::write(&content, b"untouched external state").unwrap();
            let before = fs::metadata(&content).unwrap();
            let before_ownership = metadata(&content);
            let alias = root.join(target);
            fs::create_dir_all(alias.parent().unwrap()).unwrap();
            symlink(
                if target.ends_with("gc.lock") {
                    &content
                } else {
                    &outside
                },
                alias,
            )
            .unwrap();
            let output = initialize(&root);
            assert!(!output.status.success());
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .contains("rejects staged symlinks")
            );
            let after = fs::metadata(&content).unwrap();
            assert_eq!(metadata(&content), before_ownership);
            assert_eq!(stat_snapshot(&after), stat_snapshot(&before));
            assert_eq!(fs::read(content).unwrap(), b"untouched external state");
        }
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_rejects_invalid_manifests_before_initializing_state() {
        let valid = manifest();
        let mut nul = valid.clone();
        nul.push(0);
        let mut blank = valid.clone();
        blank.push(b'\n');
        let cases = [
            None,
            Some(Vec::new()),
            Some(b"/outside\n".to_vec()),
            Some(nul),
            Some(blank),
            Some(
                String::from_utf8(valid.clone())
                    .unwrap()
                    .replace("passwd", "passwd/child")
                    .into_bytes(),
            ),
            Some(
                String::from_utf8(valid)
                    .unwrap()
                    .replace("passwd", "passwd\r")
                    .into_bytes(),
            ),
        ];
        for manifest in cases {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            write(root, "etc/wrix/private-nix-version", b"1\n");
            if let Some(manifest) = manifest {
                write(root, "etc/wrix/store-roots", &manifest);
            }
            assert!(!initialize(root).status.success());
            assert!(!root.join("nix").exists());
        }
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_refuses_manifest_alias_and_conflicting_root_without_overwriting() {
        for alias in [true, false] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            write(root, "etc/wrix/private-nix-version", b"1\n");
            let outside = root.join("outside");
            fs::write(&outside, manifest()).unwrap();
            let target = root.join("etc/wrix/store-roots");
            let directory = root.join("nix/var/nix/gcroots/wrix-image");
            let root_link = directory.join(Path::new(&roots()[0]).file_name().unwrap());
            if alias {
                symlink(&outside, target).unwrap();
            } else {
                fs::write(target, manifest()).unwrap();
                fs::create_dir_all(directory).unwrap();
                symlink(&outside, &root_link).unwrap();
            }
            assert!(!initialize(root).status.success());
            assert_eq!(fs::read(&outside).unwrap(), manifest());
            if !alias {
                assert_eq!(fs::read_link(root_link).unwrap(), outside);
            }
        }
    }

    #[test]
    #[ignore = "owning Nix fakeroot boundary"]
    fn staged_rejects_unmarked_trees_without_changing_them() {
        let temporary = tempfile::tempdir().unwrap();
        for root in [
            temporary.path().to_owned(),
            temporary.path().join("missing/stage"),
        ] {
            let output = initialize(&root);
            assert!(!output.status.success());
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .contains("requires a staged image root")
            );
            assert_eq!(temporary.path().read_dir().unwrap().count(), 0);
        }
    }
}
