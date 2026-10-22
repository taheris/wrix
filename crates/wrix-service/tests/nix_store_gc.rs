//! Collection scheduling and actual filesystem exclusion, not native Nix/runtime acceptance.

#![cfg(test)]

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io,
    sync::Mutex,
    time::{Duration, Instant},
};
use wrix_core::nix_store::pool::{Key, Repository, System};
use wrix_service::nix_store::gc::{self, Attempt, Backend, Failure, Outcome, Wait};

fn repository() -> Repository {
    Repository::try_from("a".repeat(32)).unwrap()
}

struct Filesystem {
    directory: tempfile::TempDir,
    inventory: Vec<Key>,
    acquired: Mutex<Vec<System>>,
    collected: Mutex<Vec<System>>,
    failed: Option<System>,
    missing: Option<System>,
    inventory_error: bool,
}

struct Guard {
    file: File,
    pool: Key,
}

impl Filesystem {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            inventory: [System::X86_64Linux, System::Aarch64Linux]
                .map(|system| Key {
                    repository: repository(),
                    system,
                })
                .to_vec(),
            acquired: Mutex::new(Vec::new()),
            collected: Mutex::new(Vec::new()),
            failed: None,
            missing: None,
            inventory_error: false,
        }
    }

    fn file(&self, system: System) -> File {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.path().join(system.as_str()))
            .unwrap()
    }
}

impl Backend for Filesystem {
    type Guard = Guard;
    type Error = io::Error;

    fn pools(&self, selected: &Repository) -> io::Result<Vec<Key>> {
        assert_eq!(selected, &repository());
        if self.inventory_error {
            return Err(io::Error::other("owner inventory unavailable"));
        }
        Ok(self.inventory.clone())
    }

    fn acquire(&self, pool: &Key) -> Attempt<Guard, io::Error> {
        self.acquired.lock().unwrap().push(pool.system);
        if self.missing == Some(pool.system) {
            return Attempt::Unavailable(io::Error::other("owner unavailable"));
        }
        let file = self.file(pool.system);
        match file.try_lock_exclusive() {
            Ok(()) => Attempt::Acquired(Guard {
                file,
                pool: pool.clone(),
            }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Attempt::Busy,
            Err(error) => Attempt::Unavailable(error),
        }
    }

    fn collect(&self, guard: Guard) -> io::Result<()> {
        let contender = self.file(guard.pool.system);
        assert_eq!(
            contender.try_lock_exclusive().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        self.collected.lock().unwrap().push(guard.pool.system);
        if self.failed == Some(guard.pool.system) {
            return Err(io::Error::other("native collection failed"));
        }
        drop(guard.file);
        contender.try_lock_exclusive()?;
        Ok(())
    }
}

#[test]
fn wait_is_per_invocation_defaults_to_thirty_seconds_and_preserves_precision() {
    assert_eq!(Wait::default().duration(), Duration::from_secs(30));
    assert_eq!(Wait::parse("0s").unwrap().duration(), Duration::ZERO);
    assert_eq!(
        Wait::parse("1us").unwrap().duration(),
        Duration::from_micros(1)
    );
    assert_eq!(
        Wait::parse("1m 2s").unwrap().duration(),
        Duration::from_secs(62)
    );
    for invalid in ["", "-1s", "forever", "18446744073709551615s"] {
        assert!(Wait::parse(invalid).is_err(), "{invalid}");
    }
    assert_eq!(Wait::default().duration(), Duration::from_secs(30));
}

#[test]
fn zero_wait_attempts_busy_and_free_units_once_without_breaking_exclusion() {
    let backend = Filesystem::new();
    let lock = backend.file(System::X86_64Linux);
    lock.lock_exclusive().unwrap();
    let report = gc::run(&backend, &repository(), Wait::parse("0s").unwrap()).unwrap();
    assert!(!report.complete());
    assert_eq!(
        report.units[0].outcome,
        Outcome::Incomplete(Failure::Contended)
    );
    assert_eq!(report.units[1].outcome, Outcome::Collected);
    assert_eq!(
        *backend.acquired.lock().unwrap(),
        [System::X86_64Linux, System::Aarch64Linux]
    );
    assert_eq!(*backend.collected.lock().unwrap(), [System::Aarch64Linux]);
    assert_eq!(
        backend
            .file(System::X86_64Linux)
            .try_lock_exclusive()
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn timely_native_lock_release_allows_collection_without_restarting_the_unit_budget() {
    let backend = Filesystem::new();
    let lock = backend.file(System::X86_64Linux);
    lock.lock_exclusive().unwrap();
    std::thread::scope(|scope| {
        let collector =
            scope.spawn(|| gc::run(&backend, &repository(), Wait::parse("2s").unwrap()).unwrap());
        while backend.acquired.lock().unwrap().is_empty() {
            std::thread::yield_now();
        }
        drop(lock);
        assert!(collector.join().unwrap().complete());
    });
    assert_eq!(
        *backend.collected.lock().unwrap(),
        [System::X86_64Linux, System::Aarch64Linux]
    );
}

#[test]
fn each_contended_unit_exhausts_one_budget_and_remaining_units_are_attempted() {
    let backend = Filesystem::new();
    let locks = [System::X86_64Linux, System::Aarch64Linux].map(|system| {
        let file = backend.file(system);
        file.lock_exclusive().unwrap();
        file
    });
    let started = Instant::now();
    let report = gc::run(&backend, &repository(), Wait::parse("20ms").unwrap()).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(40));
    assert_eq!(report.units.len(), 2);
    assert!(
        report
            .units
            .iter()
            .all(|unit| unit.outcome == Outcome::Incomplete(Failure::Contended))
    );
    assert!(backend.collected.lock().unwrap().is_empty());
    drop(locks);
    assert!(
        gc::run(&backend, &repository(), Wait::parse("0s").unwrap())
            .unwrap()
            .complete()
    );
}

#[test]
fn failed_collection_preserves_the_failure_and_collects_the_independent_pool() {
    let mut backend = Filesystem::new();
    backend.failed = Some(System::X86_64Linux);
    let report = gc::run(&backend, &repository(), Wait::parse("0s").unwrap()).unwrap();
    assert!(!report.complete());
    assert_eq!(
        report.units[0].outcome,
        Outcome::Incomplete(Failure::Collection("native collection failed".into()))
    );
    assert_eq!(report.units[1].outcome, Outcome::Collected);
    assert_eq!(backend.collected.lock().unwrap().len(), 2);
}

#[test]
fn unavailable_owner_is_not_collected_and_does_not_hide_other_collection_failure() {
    let mut backend = Filesystem::new();
    backend.failed = Some(System::X86_64Linux);
    backend.missing = Some(System::Aarch64Linux);
    let report = gc::run(&backend, &repository(), Wait::parse("0s").unwrap()).unwrap();
    assert!(!report.complete());
    assert_eq!(
        report.units[0].outcome,
        Outcome::Incomplete(Failure::Collection("native collection failed".into()))
    );
    assert_eq!(
        report.units[1].outcome,
        Outcome::Incomplete(Failure::Unavailable("owner unavailable".into()))
    );
    assert_eq!(*backend.collected.lock().unwrap(), [System::X86_64Linux]);
}

#[test]
fn failed_inventory_is_not_treated_as_an_empty_scope_or_allowed_to_acquire() {
    let mut backend = Filesystem::new();
    backend.inventory.clear();
    backend.inventory_error = true;
    assert!(matches!(
        gc::run(&backend, &repository(), Wait::default()),
        Err(gc::Error::Inventory(_))
    ));
    assert!(backend.acquired.lock().unwrap().is_empty());
}

#[test]
fn peer_or_duplicate_inventory_refuses_all_collection_before_acquisition() {
    for duplicate in [false, true] {
        let mut backend = Filesystem::new();
        if duplicate {
            backend.inventory.push(backend.inventory[0].clone());
        } else {
            backend.inventory[1].repository = Repository::try_from("b".repeat(32)).unwrap();
        }
        assert!(matches!(
            gc::run(&backend, &repository(), Wait::default()),
            Err(gc::Error::Scope)
        ));
        assert!(backend.acquired.lock().unwrap().is_empty());
        assert!(backend.collected.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(backend.directory.path()).unwrap().count(),
            0
        );
    }
}

#[test]
fn only_positively_empty_inventory_succeeds_without_backend_access() {
    let mut backend = Filesystem::new();
    backend.inventory.clear();
    let report = gc::run(&backend, &repository(), Wait::default()).unwrap();
    assert!(report.complete());
    assert_eq!(report.units, []);
    assert!(backend.acquired.lock().unwrap().is_empty());
}
