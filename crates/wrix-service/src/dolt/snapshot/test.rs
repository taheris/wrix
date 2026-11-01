use super::*;
use std::{fs, os::unix::fs::symlink};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MANIFEST: &str = "5:__DOLT__:0123456789abcdefghijklmnopqrstuv:00000000000000000000000000000000:00000000000000000000000000000000";

fn open(path: &Path) -> Result<File, Error> {
    Ok(File::from(at::open(path, DIRECTORY, Mode::empty())?))
}

#[test]
fn invalid_manifest_cannot_replace_last_usable_private_manifest() -> TestResult {
    let root = tempfile::tempdir()?;
    let private = root.path().join("native-remote");
    let projection = root.path().join("beads-sync/beads/.beads/dolt-remote");
    fs::create_dir_all(&private)?;
    fs::create_dir_all(&projection)?;
    fs::write(private.join("manifest"), MANIFEST)?;
    let inode = fs::metadata(private.join("manifest"))?.ino();
    let invalid = [
        format!("<<<<<<< HEAD\n{MANIFEST}\n=======\n{MANIFEST}\n>>>>>>> incoming\n"),
        String::new(),
        "snapshot".into(),
        MANIFEST.replacen("5:", "6:", 1),
        MANIFEST.replace("__DOLT__", "unknown"),
        format!("{MANIFEST}:{}:1", "d".repeat(32)),
        MANIFEST.replace("0123456789abcdefghijklmnopqrstuv", "invalid-hash"),
        format!("{MANIFEST}:0123456789abcdefghijklmnopqrstuv"),
        format!("{MANIFEST}:0123456789abcdefghijklmnopqrstuv:4294967296"),
        format!("{MANIFEST}:0123456789abcdefghijklmnopqrstuv:-1"),
        format!("{MANIFEST}\n"),
    ];
    for bytes in invalid {
        fs::write(projection.join("manifest"), bytes)?;
        assert!(transfer(root.path(), &Branch::parse("beads")?, false).is_err());
        assert_eq!(fs::read(private.join("manifest"))?, MANIFEST.as_bytes());
        assert_eq!(fs::metadata(private.join("manifest"))?.ino(), inode);
        assert!(!fs::read_dir(&private)?.any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wrix-snapshot-")
        }));
    }
    // Retained private state is still publishable after every rejected import.
    transfer(root.path(), &Branch::parse("beads")?, true)?;
    assert_eq!(fs::read(projection.join("manifest"))?, MANIFEST.as_bytes());
    Ok(())
}

#[test]
fn supported_manifest_versions_and_table_counts_roundtrip() -> TestResult {
    for bytes in [
        MANIFEST.to_owned(),
        format!("4:__DOLT__:{}:{}", "a".repeat(32), "0".repeat(32)),
        format!(
            "{MANIFEST}:{}:4294967295:{}:0",
            "b".repeat(32),
            "c".repeat(32)
        ),
    ] {
        let root = tempfile::tempdir()?;
        let private = root.path().join("native-remote");
        fs::create_dir(&private)?;
        fs::write(private.join("manifest"), &bytes)?;
        fs::write(private.join("b".repeat(32)), "table")?;
        fs::write(private.join(format!("{}.darc", "c".repeat(32))), "archive")?;
        transfer(root.path(), &Branch::parse("beads")?, true)?;
        transfer(root.path(), &Branch::parse("beads")?, false)?;
        assert_eq!(fs::read(private.join("manifest"))?, bytes.as_bytes());
    }
    Ok(())
}

#[test]
fn native_producer_snapshot_survives_conflicted_import_and_subsequent_push() -> TestResult {
    let root = tempfile::tempdir()?;
    let database = root.path().join("database");
    fs::create_dir(&database)?;
    let native = |arguments: &[&str]| -> TestResult {
        let output = std::process::Command::new("dolt")
            .args(arguments)
            .current_dir(&database)
            .env("HOME", root.path())
            .env_remove("DOLT_ROOT_HOST")
            .env_remove("DOLT_ROOT_PASSWORD")
            .env_remove("DOLT_CLI_PASSWORD")
            .stdin(std::process::Stdio::null())
            .output()?;
        assert!(output.status.success(), "{output:?}");
        Ok(())
    };
    native(&[
        "init",
        "--name",
        "Fixture",
        "--email",
        "fixture@example.invalid",
    ])?;
    native(&[
        "sql",
        "-q",
        "CREATE TABLE retained (id INT PRIMARY KEY); INSERT INTO retained VALUES (1)",
    ])?;
    native(&["add", "."])?;
    native(&["commit", "-m", "retained data"])?;
    let private = root.path().join("native-remote");
    native(&[
        "remote",
        "add",
        "origin",
        &format!("file://{}", private.display()),
    ])?;
    native(&["push", "origin", "main"])?;
    let branch = Branch::parse("beads")?;
    transfer(root.path(), &branch, true)?;
    transfer(root.path(), &branch, false)?;
    let retained = fs::read(private.join("manifest"))?;
    let projection = root
        .path()
        .join("beads-sync/beads/.beads/dolt-remote/manifest");
    fs::write(
        &projection,
        format!(
            "<<<<<<< HEAD\n{}\n=======\ninvalid\n>>>>>>> incoming",
            String::from_utf8_lossy(&retained)
        ),
    )?;
    assert!(transfer(root.path(), &branch, false).is_err());
    assert_eq!(fs::read(private.join("manifest"))?, retained);
    native(&["sql", "-q", "INSERT INTO retained VALUES (2)"])?;
    native(&["add", "."])?;
    native(&["commit", "-m", "subsequent data"])?;
    native(&["push", "origin", "main"])?;
    transfer(root.path(), &branch, true)?;
    transfer(root.path(), &branch, false)?;
    Ok(())
}

#[test]
fn absent_manifest_is_not_a_successful_stale_snapshot() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::create_dir_all(root.path().join("native-remote"))?;
    fs::create_dir_all(root.path().join("beads-sync/beads/.beads/dolt-remote"))?;
    fs::write(root.path().join("native-remote/manifest"), "retained")?;
    assert!(transfer(root.path(), &Branch::parse("beads")?, false).is_err());
    assert_eq!(
        fs::read(root.path().join("native-remote/manifest"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn projection_ancestor_and_leaf_symlinks_never_touch_peer() -> TestResult {
    let root = tempfile::tempdir()?;
    let peer = tempfile::tempdir()?;
    fs::write(peer.path().join("manifest"), "peer")?;
    fs::create_dir_all(root.path().join("beads-sync/beads/.beads"))?;
    let projection = root.path().join("beads-sync/beads/.beads/dolt-remote");
    symlink(peer.path(), &projection)?;
    assert!(transfer(root.path(), &Branch::parse("beads")?, false).is_err());
    assert!(transfer(root.path(), &Branch::parse("beads")?, true).is_err());
    fs::remove_file(&projection)?;
    fs::create_dir(&projection)?;
    symlink(peer.path().join("manifest"), projection.join("manifest"))?;
    assert!(transfer(root.path(), &Branch::parse("beads")?, false).is_err());
    assert_eq!(fs::read(peer.path().join("manifest"))?, b"peer");
    Ok(())
}

#[test]
fn native_lock_inode_is_preserved_and_writer_contention_is_bounded() -> TestResult {
    let root = tempfile::tempdir()?;
    let branch = Branch::parse("beads")?;
    let private = root.path().join("native-remote");
    let projection = root.path().join("beads-sync/beads/.beads/dolt-remote");
    fs::create_dir_all(&private)?;
    fs::create_dir_all(&projection)?;
    fs::write(private.join("manifest"), MANIFEST)?;
    fs::write(private.join("LOCK"), "native")?;
    fs::write(projection.join("LOCK"), "retained")?;
    let private_inode = fs::metadata(private.join("LOCK"))?.ino();
    let projected_inode = fs::metadata(projection.join("LOCK"))?.ino();
    transfer(root.path(), &branch, true)?;
    transfer(root.path(), &branch, false)?;
    assert_eq!(fs::metadata(private.join("LOCK"))?.ino(), private_inode);
    assert_eq!(
        fs::metadata(projection.join("LOCK"))?.ino(),
        projected_inode
    );
    let lock = File::options()
        .read(true)
        .write(true)
        .open(private.join("LOCK"))?;
    lock.try_lock_exclusive()?;
    assert!(transfer(root.path(), &branch, true).is_err());
    Ok(())
}

#[test]
fn special_files_hardlinks_and_byte_entry_depth_budgets_fail_before_manifest() -> TestResult {
    for hostile in ["fifo", "hardlink", "bytes", "entries", "depth"] {
        let source = tempfile::tempdir()?;
        let destination = tempfile::tempdir()?;
        fs::write(source.path().join("manifest"), MANIFEST)?;
        fs::write(destination.path().join("manifest"), "retained")?;
        match hostile {
            "fifo" => nix::unistd::mkfifo(
                &source.path().join("hostile"),
                nix::sys::stat::Mode::S_IRUSR,
            )?,
            "hardlink" => fs::hard_link(
                source.path().join("manifest"),
                source.path().join("hostile"),
            )?,
            "depth" => fs::create_dir_all(source.path().join("d/".repeat(34)))?,
            _ => fs::write(source.path().join("hostile"), "too many bytes")?,
        }
        let mut budget = Budget {
            bytes: if hostile == "bytes" { 1 } else { 1024 },
            entries: if hostile == "entries" { 1 } else { 100 },
        };
        assert!(
            copy(
                &open(source.path())?,
                &open(destination.path())?,
                &mut budget,
                0
            )
            .is_err(),
            "{hostile}"
        );
        assert_eq!(
            fs::read(destination.path().join("manifest"))?,
            b"retained",
            "{hostile}"
        );
    }
    Ok(())
}

#[test]
fn retained_source_descriptor_cannot_copy_peer_through_concurrent_symlink_swaps() -> TestResult {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let original = tempfile::tempdir()?;
    let peer = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    fs::create_dir(original.path().join("data"))?;
    fs::write(original.path().join("data/value"), "own")?;
    fs::write(peer.path().join("value"), "peer-secret")?;
    let retained = open(original.path())?;
    let peer_path = peer.path().to_owned();
    let running = Arc::new(AtomicBool::new(true));
    let racing = running.clone();
    let worker = std::thread::spawn(move || -> std::io::Result<()> {
        while racing.load(Ordering::Relaxed) {
            at::renameat(&retained, "data", &retained, "retained-data")?;
            at::symlinkat(&peer_path, &retained, "data")?;
            std::thread::yield_now();
            at::unlinkat(&retained, "data", at::AtFlags::empty())?;
            at::renameat(&retained, "retained-data", &retained, "data")?;
        }
        Ok(())
    });
    for index in 0..30 {
        let target = private.path().join(index.to_string());
        // Transient disappearance/symlink races reject. Successful snapshots
        // can contain only opened own files, never a followed peer directory.
        let _result = source_snapshot(original.path(), &target);
        for directory in ["data", "retained-data"] {
            let value = target.join(directory).join("value");
            if value.is_file() {
                assert_eq!(fs::read(value)?, b"own");
            }
        }
    }
    running.store(false, Ordering::Relaxed);
    worker.join().unwrap()?;
    Ok(())
}

#[test]
fn destination_symlink_is_replaced_not_followed() -> TestResult {
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let peer = tempfile::NamedTempFile::new()?;
    fs::write(source.path().join("manifest"), MANIFEST)?;
    fs::write(source.path().join("table"), "own")?;
    fs::write(peer.path(), "peer")?;
    symlink(peer.path(), destination.path().join("table"))?;
    copy(
        &open(source.path())?,
        &open(destination.path())?,
        &mut Budget {
            bytes: 1024,
            entries: 100,
        },
        0,
    )?;
    assert_eq!(fs::read(peer.path())?, b"peer");
    assert_eq!(fs::read(destination.path().join("table"))?, b"own");
    Ok(())
}
