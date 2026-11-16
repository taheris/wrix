use super::*;
use rusqlite::Connection;
use std::{sync::mpsc, thread, time::Instant};

fn database(root: &Path) -> (std::path::PathBuf, Connection) {
    let path = root.join("fetcher-cache-v4.sqlite");
    let database = Connection::open(&path).unwrap();
    database
        .execute_batch("CREATE TABLE metadata(value TEXT); INSERT INTO metadata VALUES ('before')")
        .unwrap();
    (path, database)
}

#[test]
fn waits_for_writer_and_publishes_a_consistent_standalone_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let (source, database) = database(root.path());
    let destination = root.path().join("snapshot.sqlite");
    let (ready, received) = mpsc::channel();
    let writer = thread::spawn(move || {
        database
            .execute_batch("BEGIN EXCLUSIVE; UPDATE metadata SET value='after'")
            .unwrap();
        ready.send(()).unwrap();
        thread::sleep(Duration::from_millis(200));
        database.execute_batch("COMMIT").unwrap();
    });
    received.recv().unwrap();
    snapshot(&source, &destination).unwrap();
    writer.join().unwrap();
    let captured = Connection::open(&destination).unwrap();
    let value: String = captured
        .query_row("SELECT value FROM metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "after");
    let integrity: String = captured
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn wal_snapshot_excludes_uncommitted_writes_without_changing_the_writer() {
    let root = tempfile::tempdir().unwrap();
    let (source, writer) = database(root.path());
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; UPDATE metadata SET value='pending'",
        )
        .unwrap();
    let destination = root.path().join("snapshot.sqlite");
    snapshot(&source, &destination).unwrap();
    let captured = Connection::open(destination).unwrap();
    let value: String = captured
        .query_row("SELECT value FROM metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "before");
    let pending: String = writer
        .query_row("SELECT value FROM metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pending, "pending");
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn persistent_lock_fails_boundedly_without_publishing_partial_data() {
    let root = tempfile::tempdir().unwrap();
    let (source, database) = database(root.path());
    database.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let destination = root.path().join("snapshot.sqlite");
    let start = Instant::now();
    let result = copy(&source, &destination, Duration::from_millis(50));
    assert!(matches!(result, Err(Error::Sql(_))));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!destination.exists());
    database.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn refuses_existing_destinations_corrupt_sources_and_non_cache_databases() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fetcher-cache-v4.sqlite");
    let destination = root.path().join("snapshot.sqlite");
    fs::write(&source, "not sqlite").unwrap();
    assert!(snapshot(&source, &destination).is_err());
    assert!(!destination.exists());
    fs::write(&destination, "preserve").unwrap();
    assert!(matches!(
        snapshot(&source, &destination),
        Err(Error::Exists)
    ));
    assert_eq!(fs::read_to_string(&destination).unwrap(), "preserve");
    assert!(matches!(
        snapshot(&root.path().join("service.sqlite"), &destination),
        Err(Error::Name)
    ));
}
