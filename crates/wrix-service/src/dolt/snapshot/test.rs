use super::*;
use std::{fs, os::unix::fs::symlink};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn open(path: &Path) -> Result<File, Error> {
    Ok(File::from(at::open(path, DIRECTORY, Mode::empty())?))
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
    fs::write(private.join("manifest"), "snapshot")?;
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
        fs::write(source.path().join("manifest"), "new")?;
        fs::write(destination.path().join("manifest"), "retained")?;
        match hostile {
            "fifo" => at::mknodat(
                open(source.path())?,
                "hostile",
                at::FileType::Fifo,
                Mode::RUSR,
                0,
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
    fs::write(source.path().join("manifest"), "new")?;
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
