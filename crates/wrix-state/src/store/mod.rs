//! Host-private control metadata; payloads, secrets and live ownership stay outside SQLite.

pub mod admission;
mod record;
mod schema;

use schema::DDL;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

use displaydoc::Display;
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use thiserror::Error;

use crate::{host::Host, integration::Slot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Generation([u8; 32]);

const APPLICATION: i64 = 0x5752_5831;
const SCHEMA: i64 = 1;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// {0}
    Host(#[from] crate::host::Error),
    /// {0}
    Integration(#[from] crate::integration::Error),
    /// {0}
    Resolver(#[from] crate::resolver::Error),
    /// host state I/O failed: {0}
    Io(#[from] io::Error),
    /// native metadata import failed: {0}
    Document(#[from] crate::namespace::document::ParseError),
    /// host state writer is busy; retry after the active operation finishes
    Busy,
    /// host state database failed: {0}
    Sql(#[from] rusqlite::Error),
    /// host state must be owned, private and free of symlink substitutions
    PrivateState,
    /// missing or unsupported host state; initialize explicitly and do not reset existing authority
    Unsupported,
    /// host admission is not initialized; run wrix workspace initialize
    Uninitialized,
    /// native workspace paths differ from the selected host roots; restore the owning HOME/XDG environment instead of using filesystem exports
    RootMismatch,
    /// native namespace {0} needs a fresh authorized ordinary command from its checkout before background publication
    ScopeRequired(wrix_core::path::WorkspaceHash),
    /// native namespace {0} requires explicit wrix workspace import-native --native-key {0}
    ImportRequired(wrix_core::path::WorkspaceHash),
    /// conflicting workspace admission or native-key reservation
    Conflict,
    /// standalone namespace creation requires wrix workspace acknowledge-scope --version 1
    Acknowledgment,
    /// invalid workspace input: {0}
    Invalid(String),
}

pub struct Store {
    pub(super) connection: Connection,
    _lock: Option<File>,
    _root_lock: Option<File>,
}

impl Store {
    pub fn inspect(host: &Host) -> Result<Option<Self>, Error> {
        host.check()?;
        let path = host.roots().database();
        if !exists(&path)? {
            if exists(&host.roots().state().join("state.lock"))? {
                return Err(Error::Unsupported);
            }
            return Ok(None);
        }
        host.private_root(false)?;
        private_file(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        connection.pragma_update(None, "query_only", true)?;
        validate(&connection)?;
        Ok(Some(Self {
            connection,
            _lock: None,
            _root_lock: None,
        }))
    }

    pub(super) fn mutate(host: &Host, create: bool) -> Result<Self, Error> {
        host.private_root(create)?;
        let path = host.roots().database();
        let lock_path = host.roots().state().join("state.lock");
        if !create && !exists(&path)? {
            return Err(Error::Uninitialized);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let root_lock = OpenOptions::new()
            .read(true)
            .custom_flags(
                i32::try_from(
                    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::DIRECTORY).bits(),
                )
                .map_err(|_| Error::PrivateState)?,
            )
            .open(host.roots().state())?;
        acquire(&root_lock, deadline)?;
        let existed = exists(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
                    .map_err(|_| Error::PrivateState)?,
            )
            .open(&lock_path)?;
        private_file(&lock_path)?;
        acquire(&lock, deadline)?;
        if !exists(&path)? {
            if !create || existed {
                return Err(Error::Unsupported);
            }
            publish(host)?;
        }
        private_file(&path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        validate(&connection)?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(Self {
            connection,
            _lock: Some(lock),
            _root_lock: Some(root_lock),
        })
    }

    pub fn initialize(host: &Host) -> Result<(), Error> {
        let mut store = Self::mutate(host, true)?;
        if store.initialized()? {
            return Ok(());
        }
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE authority SET admission_initialized = 1 WHERE singleton = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn import_native(host: &Host, key: &wrix_core::path::WorkspaceHash) -> Result<(), Error> {
        let mut store = Self::mutate(host, false)?;
        crate::namespace::import(host, &mut store, key)
    }

    pub fn acknowledge(host: &Host, version: u32) -> Result<(), Error> {
        if version != 1 {
            return Err(Error::Unsupported);
        }
        let mut store = Self::mutate(host, true)?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE authority SET scope_policy = 1 WHERE singleton = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn generation(&self) -> Result<Generation, Error> {
        let bytes: Vec<u8> = self.connection.query_row(
            "SELECT generation FROM authority WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(Generation(
            bytes.try_into().map_err(|_| Error::Unsupported)?,
        ))
    }

    pub fn initialized(&self) -> Result<bool, Error> {
        Ok(self.connection.query_row(
            "SELECT admission_initialized FROM authority WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn acknowledged(&self) -> Result<bool, Error> {
        Ok(self.connection.query_row(
            "SELECT scope_policy = 1 FROM authority WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn standalone(
        &self,
        key: &wrix_core::path::WorkspaceHash,
        allocating: bool,
    ) -> Result<(), Error> {
        let reserved: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM binding WHERE native_key = ?1)",
            [key.as_str()],
            |row| row.get(0),
        )?;
        if reserved {
            return Err(Error::Conflict);
        }
        if allocating && !self.acknowledged()? {
            return Err(Error::Acknowledgment);
        }
        Ok(())
    }

    pub fn integration(&self) -> Result<Option<(Slot, std::path::PathBuf)>, Error> {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let value: Option<(String, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT slot, executable FROM integration WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        value
            .map(|(slot, bytes)| {
                let slot = Slot::parse(&slot)?;
                let path = std::path::PathBuf::from(OsString::from_vec(bytes));
                if !path.is_absolute() {
                    return Err(Error::Unsupported);
                }
                Ok((slot, path))
            })
            .transpose()
    }

    pub fn select(host: &Host, slot: &Slot, executable: &Path) -> Result<(), Error> {
        use std::os::unix::ffi::OsStrExt;
        if !executable.is_absolute() {
            return Err(Error::Invalid(
                "integration executable must be absolute".into(),
            ));
        }
        let metadata = fs::symlink_metadata(executable)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o022 != 0
            || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
        {
            return Err(Error::PrivateState);
        }
        let mut store = Self::mutate(host, false)?;
        if !store.initialized()? {
            return Err(Error::Uninitialized);
        }
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO integration VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET slot = excluded.slot, executable = excluded.executable", rusqlite::params![slot.as_str(), executable.as_os_str().as_bytes()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn disable(host: &Host) -> Result<(), Error> {
        let mut store = Self::mutate(host, false)?;
        if !store.initialized()? {
            return Err(Error::Uninitialized);
        }
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM integration", [])?;
        tx.commit()?;
        Ok(())
    }
}

fn acquire(file: &File, deadline: std::time::Instant) -> Result<(), Error> {
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn private_file(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(Error::PrivateState);
    }
    Ok(())
}

fn validate(db: &Connection) -> Result<(), Error> {
    let application: i64 = db.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let schema: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let journal: String = db.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if application != APPLICATION || schema != SCHEMA || journal != "delete" {
        return Err(Error::Unsupported);
    }
    let valid: bool = db.query_row("SELECT count(*) = 1 AND min(length(generation)) = 32 AND min(admission_initialized) IN (0,1) AND min(scope_policy) IN (0,1) FROM authority", [], |row| row.get(0))?;
    if !valid {
        return Err(Error::Unsupported);
    }
    let mut expected: Vec<_> = DDL
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect();
    let mut query = db.prepare(
        "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'",
    )?;
    let mut actual = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    expected.sort_unstable();
    actual.sort_unstable();
    if expected != actual {
        return Err(Error::Unsupported);
    }
    if db
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(Error::Unsupported);
    }
    record::check(db)?;
    let check: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(Error::Unsupported);
    }
    Ok(())
}

fn publish(host: &Host) -> Result<(), Error> {
    let temporary = tempfile::NamedTempFile::new_in(host.roots().state())?;
    {
        let db = Connection::open(temporary.path())?;
        db.pragma_update(None, "application_id", APPLICATION)?;
        db.pragma_update(None, "user_version", SCHEMA)?;
        db.pragma_update(None, "journal_mode", "DELETE")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.execute_batch(DDL)?;
        let mut generation = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut generation)?;
        db.execute(
            "INSERT INTO authority VALUES (1, ?1, 0, 0)",
            [&generation[..]],
        )?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(host.roots().database())
        .map_err(|error| error.error)?;
    File::open(host.roots().state())?.sync_all()?;
    Ok(())
}
