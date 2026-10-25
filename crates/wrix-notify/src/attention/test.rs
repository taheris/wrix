use super::{Error, enqueue, focus, open, return_to_origin, writer};
use crate::{
    navigation::Navigator,
    process::Host,
    route::{Platform, Target},
};
use rusqlite::TransactionBehavior;
use std::{path::Path, sync::mpsc};
use std::{sync::Barrier, time::Duration};

#[test]
fn queue_apis_wait_for_host_admission_before_touching_sqlite() {
    type Operation = fn(&Path) -> Result<(), Error>;
    let operations: [Operation; 3] = [
        |directory| {
            enqueue(
                directory,
                &Target::try_from("waiting".to_owned()).unwrap(),
                Platform::Linux,
            )
        },
        |directory| {
            focus(
                directory,
                Platform::Linux,
                &Navigator::new(Host, directory.join("proc")),
            )
        },
        |directory| {
            return_to_origin(
                directory,
                Platform::Linux,
                &Navigator::new(Host, directory.join("proc")),
            )
        },
    ];
    for operation in operations {
        let root = tempfile::tempdir().unwrap();
        let db = open(root.path()).unwrap();
        let held = writer::acquire(root.path()).unwrap();
        std::thread::scope(|scope| {
            let (ready_tx, ready) = mpsc::channel();
            let (done_tx, done) = mpsc::channel();
            let directory = root.path();
            let child = scope.spawn(move || {
                ready_tx.send(()).unwrap();
                done_tx.send(operation(directory)).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            let blocked = done.recv_timeout(Duration::from_millis(100));
            db.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;").unwrap();
            drop(held);
            assert!(matches!(blocked, Err(mpsc::RecvTimeoutError::Timeout)));
            done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            child.join().unwrap();
        });
    }
}

#[test]
fn short_concurrent_writers_do_not_exhaust_sqlite_busy_backoff() {
    let root = tempfile::tempdir().unwrap();
    open(root.path()).unwrap();
    let ready = Barrier::new(60);
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..60)
            .map(|index| {
                let ready = &ready;
                let directory = root.path();
                scope.spawn(move || {
                    let mut db = open(directory).unwrap();
                    db.busy_timeout(Duration::from_secs(5)).unwrap();
                    ready.wait();
                    let _writer = writer::acquire(directory).unwrap();
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    std::thread::sleep(Duration::from_millis(10));
                    tx.execute(
                        "INSERT INTO pending VALUES (?1, '{}', ?2)",
                        rusqlite::params![index.to_string(), index],
                    )?;
                    tx.commit()
                })
            })
            .collect();
        let results: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        assert!(results.iter().all(Result::is_ok), "{results:?}");
    });
    assert_eq!(
        open(root.path())
            .unwrap()
            .query_row("SELECT count(*) FROM pending", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        60
    );
}
