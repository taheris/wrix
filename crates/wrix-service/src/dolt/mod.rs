//! Trusted native Dolt registration and fixed-remote sync; never a tenant SQL administrator.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use wrix_core::{git::Branch, path::WorkspaceHash};

use crate::control::{Authorization, Permission};

pub mod broker;
pub mod server;
mod snapshot;
mod source;

impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

/// Management transport is host-selected, never a tenant-provided command or address.
enum Executor {
    Tcp {
        port: std::num::NonZeroU16,
        password: String,
    },
    Container {
        runtime: PathBuf,
        container: String,
        data: PathBuf,
    },
}

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// shared Dolt management I/O failed: {0}
    Io(#[from] io::Error),
    /// shared Dolt management record is invalid: {0}
    Encoding(#[from] serde_json::Error),
    /// shared Dolt management requires current host SQL authorization
    Unauthorized,
    /// shared Dolt management path is not a canonical private directory
    UnsafePath,
    /// shared Dolt registration is incomplete or conflicts with retained native data; retain the database and registration backup for explicit host maintenance
    Incomplete,
    /// no configured native Beads database was found beneath {root}; no source was moved
    SourceMissing { root: PathBuf },
    /// multiple or conflicting native databases beneath {root}; select the project database in .beads/metadata.json before migration; no source was moved
    SourceAmbiguous { root: PathBuf },
    /// native Dolt writer still owns {path}; quiesce the legacy server before migration; no serving database is replaced
    SourceBusy { path: PathBuf },
    /// shared Dolt Unix endpoint is not accepting connections at {path}; run wrix service recover for coordinated endpoint recovery
    SocketUnavailable { path: PathBuf },
    /// shared Dolt Unix endpoint at {path} is still live; refusing to unlink it
    SocketBusy { path: PathBuf },
    /// native Dolt management failed; no tenant administrator privilege was granted: {stderr}
    Native { stderr: String },
    /// shared Dolt query returned an unexpected result
    Result,
    /// shared Dolt sync worker failed; generation supervision must stop the service
    Task,
    /// shared Dolt must start with immutable `secure_file_priv=/dev/null` before namespace access is provisioned
    UnsafeServer,
}

/// Closed sync operations; clients cannot choose a remote, URL, path, refspec or SQL statement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sync {
    Push,
    Pull,
}

/// Private scoped client material. Debug output never includes the password.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    database: String,
    user: String,
    password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Credentials {
    pub fn from_environment() -> Result<Self, Error> {
        let database =
            std::env::var("BEADS_DOLT_SERVER_DATABASE").map_err(|_| Error::Unauthorized)?;
        let user = std::env::var("BEADS_DOLT_SERVER_USER").map_err(|_| Error::Unauthorized)?;
        let password = std::env::var("BEADS_DOLT_PASSWORD").map_err(|_| Error::Unauthorized)?;
        let namespace = namespace_from_database(&database)?;
        if user != namespace.as_str()[..32]
            || password.len() != 64
            || !password.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Unauthorized);
        }
        Ok(Self {
            database,
            user,
            password,
        })
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

// Beads requires an alphabetic first byte and at most 64 bytes. Encode the
// first hex nibble as a..p, preserving all 256 identity bits without SQL grant
// wildcards (notably '_') or truncation/collision.
fn database_name(namespace: &WorkspaceHash) -> String {
    let hash = namespace.as_str();
    let first = hash.as_bytes()[0];
    let nibble = if first <= b'9' {
        first - b'0'
    } else {
        first - b'a' + 10
    };
    format!("{}{}", char::from(b'a' + nibble), &hash[1..])
}

fn namespace_from_database(database: &str) -> Result<WorkspaceHash, Error> {
    let first = *database.as_bytes().first().ok_or(Error::Unauthorized)?;
    if !(b'a'..=b'p').contains(&first) || database.len() != 64 || !database.is_ascii() {
        return Err(Error::Unauthorized);
    }
    let nibble = first - b'a';
    let hex = if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + nibble - 10
    };
    WorkspaceHash::parse(&format!("{}{}", char::from(hex), &database[1..]))
        .map_err(|_| Error::Unauthorized)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    namespace: crate::control::Namespace,
    #[serde(serialize_with = "serialize_branch")]
    branch: Branch,
    phase: Phase,
    credentials: Credentials,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Restoring,
    Imported,
    Ready,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Source {
    source: PathBuf,
    phase: SourcePhase,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SourcePhase {
    Moving,
    Checkpointed,
    Exported,
}

fn check_storage_tree(path: &Path) -> Result<(), Error> {
    check_directory(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            check_storage_tree(&entry.path())?;
        } else if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(Error::UnsafePath);
        }
    }
    Ok(())
}

/// Host-only management guard; independent namespaces do not share a sync lock.
/// A retained backup is a recovery checkpoint, not a second serving database.
pub struct Manager<'a> {
    authorization: &'a Authorization<'a>,
    root: PathBuf,
    data: PathBuf,
    namespace: WorkspaceHash,
    branch: Branch,
    executor: Executor,
    _lock: File,
    source_lock: std::sync::Mutex<Vec<File>>,
}

impl<'a> Manager<'a> {
    /// The host supplies its recorded loopback management endpoint and private credential.
    /// This API does not start a backend or admit readiness on a port alone.
    pub fn open(
        authorization: &'a Authorization<'a>,
        branch: Branch,
        port: std::num::NonZeroU16,
        admin_password: &str,
    ) -> Result<Self, Error> {
        let host = wrix_state::host::Host::discover().map_err(|_| Error::Unauthorized)?;
        Self::at(
            authorization,
            branch,
            host.roots().state(),
            port,
            admin_password,
        )
    }

    fn at(
        authorization: &'a Authorization<'a>,
        branch: Branch,
        host_root: &Path,
        port: std::num::NonZeroU16,
        admin_password: &str,
    ) -> Result<Self, Error> {
        Self::with_executor(
            authorization,
            branch,
            host_root,
            Executor::Tcp {
                port,
                password: admin_password.to_owned(),
            },
        )
    }

    /// Linux administration stays inside the selected container, with no host root TCP publication.
    pub fn in_container(
        authorization: &'a Authorization<'a>,
        branch: Branch,
        runtime: &Path,
        container: &str,
    ) -> Result<Self, Error> {
        let host = wrix_state::host::Host::discover().map_err(|_| Error::Unauthorized)?;
        let data = host.roots().state().join("service-data/dolt");
        Self::with_executor(
            authorization,
            branch,
            host.roots().state(),
            Executor::Container {
                runtime: runtime.to_owned(),
                container: container.to_owned(),
                data,
            },
        )
    }

    fn with_executor(
        authorization: &'a Authorization<'a>,
        branch: Branch,
        host_root: &Path,
        executor: Executor,
    ) -> Result<Self, Error> {
        if authorization.permission() != Permission::Sql {
            return Err(Error::Unauthorized);
        }
        let namespace = WorkspaceHash::parse(&String::from(authorization.namespace().clone()))
            .map_err(|_| Error::Unauthorized)?;
        let service = host_root.join("service-data/dolt");
        let data = service.join("namespaces").join(namespace.as_str());
        let root = host_root.join("host-control/dolt").join(namespace.as_str());
        private_directory(&root)?;
        private_directory(&data)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(data.join("sync.lock"))?;
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::metadata(&root)?.uid()
        {
            return Err(Error::UnsafePath);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self {
            authorization,
            root,
            data,
            namespace,
            branch,
            executor,
            _lock: lock,
            source_lock: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Resolve only a native database under this admitted workspace. On restart,
    /// journal provenance is used instead of rescanning a retired repository path.
    pub fn prepare_workspace_source(&self, workspace: &Path) -> Result<(), Error> {
        if self.read_record()?.is_some() {
            return Ok(());
        }
        if let Some(saved) = self.source_record()? {
            if !saved.source.starts_with(workspace.join(".beads/dolt")) {
                return Err(Error::UnsafePath);
            }
            return self.prepare_source(&saved.source);
        }
        self.prepare_source(&source::locate(workspace)?)
    }

    fn source_record(&self) -> Result<Option<Source>, Error> {
        let file = match File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join("source.json"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != fs::metadata(&self.root)?.uid()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.len() > 16384
        {
            return Err(Error::UnsafePath);
        }
        let mut bytes = Vec::new();
        file.take(16385).read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err(Error::UnsafePath);
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Exclude native writers before checkpoint cutover. The same native lock
    /// inode survives rename; backup opens the fenced checkpoint read-only.
    pub fn prepare_source(&self, source: &Path) -> Result<(), Error> {
        if self.read_record()?.is_some() {
            return Ok(());
        }
        let provenance = self.root.join("source.json");
        let checkpoint = self.root.join("source-checkpoint");
        let mut saved = self.source_record()?.unwrap_or_else(|| Source {
            source: source.to_owned(),
            phase: SourcePhase::Moving,
        });
        if saved.source != source {
            return Err(Error::Incomplete);
        }
        check_directory(source)?;
        check_directory(&checkpoint)?;
        if source.exists() && checkpoint.exists() {
            return Err(Error::Incomplete);
        }
        let current = if checkpoint.exists() {
            &checkpoint
        } else {
            source
        };
        check_storage_tree(&current.join(".dolt"))?;
        let mut guards = Vec::new();
        if let Some(parent) = source.parent()
            && parent.join(".dolt/noms/LOCK").exists()
        {
            check_directory(&parent.join(".dolt/noms"))?;
            let parent_lock = File::options()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(parent.join(".dolt/noms/LOCK"))?;
            let metadata = parent_lock.metadata()?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != fs::metadata(&self.root)?.uid()
            {
                return Err(Error::UnsafePath);
            }
            parent_lock.try_lock_exclusive().map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    Error::SourceBusy {
                        path: parent.to_path_buf(),
                    }
                } else {
                    Error::Io(error)
                }
            })?;
            guards.push(parent_lock);
        }
        let lock = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(current.join(".dolt/noms/LOCK"))
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("native source lock {}: {error}", current.display()),
                )
            })?;
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != fs::metadata(&self.root)?.uid()
        {
            return Err(Error::UnsafePath);
        }
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                Error::SourceBusy {
                    path: current.to_path_buf(),
                }
            } else {
                Error::Io(error)
            }
        })?;
        if !checkpoint.exists() {
            if saved.phase != SourcePhase::Moving {
                return Err(Error::Incomplete);
            }
            atomic_json(&provenance, &saved)?;
            fs::rename(source, &checkpoint)?;
            File::open(source.parent().ok_or(Error::UnsafePath)?)?.sync_all()?;
            File::open(&self.root)?.sync_all()?;
        }
        // Recheck after moving out of the worker-visible repository. In
        // particular the fenced inode must still be the native writer lock.
        check_storage_tree(&checkpoint.join(".dolt"))?;
        let retained = fs::symlink_metadata(checkpoint.join(".dolt/noms/LOCK"))?;
        if retained.dev() != metadata.dev() || retained.ino() != metadata.ino() {
            return Err(Error::UnsafePath);
        }
        saved.phase = SourcePhase::Checkpointed;
        atomic_json(&provenance, &saved)?;
        let snapshot = self.root.join("source-snapshot");
        check_directory(&snapshot)?;
        if !snapshot.exists() {
            let building = self.root.join("source-snapshot-building");
            check_directory(&building)?;
            if building.exists() {
                fs::remove_dir_all(&building)?;
            }
            snapshot::source_snapshot(&checkpoint, &building)?;
            fs::rename(&building, &snapshot)?;
            File::open(&self.root)?.sync_all()?;
        }
        // This graph contains only fresh private inodes, not the old sandbox's
        // retained descriptors, native LOCK or auto-connect metadata.
        let private_lock = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(snapshot.join(".dolt/noms/LOCK"));
        let private_lock = match private_lock {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => File::options()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(snapshot.join(".dolt/noms/LOCK"))?,
            Err(error) => return Err(error.into()),
        };
        private_lock
            .try_lock_exclusive()
            .map_err(|_| Error::Incomplete)?;
        let output = Command::new("timeout")
            .args([
                "--signal=KILL",
                "120",
                "dolt",
                "backup",
                "sync-url",
                &self.backup_url(),
            ])
            .current_dir(&snapshot)
            .env_remove("DOLT_CLI_PASSWORD")
            .env_remove("DOLT_ROOT_PASSWORD")
            .env_remove("DOLT_ROOT_HOST")
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Incomplete);
        }
        saved.phase = SourcePhase::Exported;
        atomic_json(&provenance, &saved)?;
        // Keep the native writer guard until export and cutover are durable.
        guards.push(lock);
        *self.source_lock.lock().map_err(|_| Error::Incomplete)? = guards;
        Ok(())
    }

    /// Restore the fixed namespace backup into an already-running shared server.
    /// Native backup restore preserves committed, staged and unstaged roots and all refs.
    /// An interrupted uncertain restore is never retried over an existing database.
    pub fn register(&self) -> Result<Credentials, Error> {
        self.require_hardened_server()?;
        let existing = self.read_record()?;
        let mut record = if let Some(record) = existing {
            record
        } else {
            if self.database_exists()? || !self.data.join("registration-backup").is_dir() {
                return Err(Error::Incomplete);
            }
            let credentials = Credentials {
                database: self.database(),
                user: self.user(),
                password: random_password()?,
            };
            let record = Record {
                version: 1,
                namespace: crate::control::Namespace::from(&self.namespace),
                branch: self.branch.clone(),
                phase: Phase::Restoring,
                credentials,
            };
            check_directory(&self.data.join("registration-backup"))?;
            self.write_record(&record)?;
            self.admin(
                None,
                &format!(
                    "CALL DOLT_BACKUP('restore', {}, {})",
                    literal(&self.backup_url()),
                    literal(&self.database())
                ),
            )?;
            let mut record = record;
            record.phase = Phase::Imported;
            self.write_record(&record)?;
            record
        };
        if record.phase == Phase::Restoring || !self.database_exists()? {
            return Err(Error::Incomplete);
        }
        self.provision(&record.credentials)?;
        self.canonical_remote()?;
        self.query(
            Some(&record.credentials),
            Some(&record.credentials.database),
            "SELECT 1 AS ready",
        )?;
        record.phase = Phase::Ready;
        self.write_record(&record)?;
        atomic_json(
            &self.data.join("access.json"),
            &broker::Access {
                credentials: record.credentials.clone(),
                branch: self.branch.clone(),
                enabled: true,
            },
        )?;
        Ok(record.credentials)
    }

    /// Execute only against the selected namespace's canonical file remote.
    /// Scoped SQL users remain unable to call Dolt's `AdminOnly` remote procedures.
    pub fn sync(&self, operation: Sync) -> Result<(), Error> {
        let record = self.read_record()?.ok_or(Error::Incomplete)?;
        if record.phase != Phase::Ready || self.authorization.permission() != Permission::Sql {
            return Err(Error::Incomplete);
        }
        self.require_hardened_server()?;
        self.canonical_remote()?;
        if operation == Sync::Pull {
            snapshot::transfer(&self.data, &self.branch, false)?;
        }
        let query = match operation {
            Sync::Push => "CALL DOLT_PUSH('origin', active_branch())",
            Sync::Pull => "CALL DOLT_PULL('origin', active_branch())",
        };
        self.admin(Some(&self.database()), query)?;
        if operation == Sync::Push {
            snapshot::transfer(&self.data, &self.branch, true)?;
        }
        Ok(())
    }

    pub fn retire(&self) -> Result<(), Error> {
        let record = self.read_record()?.ok_or(Error::Incomplete)?;
        atomic_json(
            &self.data.join("access.json"),
            &broker::Access {
                credentials: record.credentials.clone(),
                branch: self.branch.clone(),
                enabled: false,
            },
        )?;
        self.admin(
            None,
            &format!(
                "DROP USER IF EXISTS {}@'%'",
                literal(record.credentials.user())
            ),
        )?;
        Ok(())
    }

    pub fn credentials(&self) -> Result<Credentials, Error> {
        let record = self.read_record()?.ok_or(Error::Incomplete)?;
        if record.phase != Phase::Ready {
            return Err(Error::Incomplete);
        }
        Ok(record.credentials)
    }

    fn database(&self) -> String {
        database_name(&self.namespace)
    }

    fn user(&self) -> String {
        self.namespace.as_str()[..32].to_owned()
    }

    fn backup_url(&self) -> String {
        format!("file://{}", self.data.join("registration-backup").display())
    }

    fn remote_url(&self) -> String {
        format!("file://{}", self.data.join("native-remote").display())
    }

    fn require_hardened_server(&self) -> Result<(), Error> {
        let rows = self.admin(None, "SELECT @@GLOBAL.secure_file_priv AS secure_file_priv")?;
        if rows
            .first()
            .and_then(|row| row.get("secure_file_priv"))
            .and_then(serde_json::Value::as_str)
            != Some("/dev/null")
        {
            return Err(Error::UnsafeServer);
        }
        Ok(())
    }

    fn database_exists(&self) -> Result<bool, Error> {
        let rows = self.admin(
            None,
            &format!(
                "SELECT SCHEMA_NAME FROM information_schema.schemata WHERE SCHEMA_NAME = {}",
                literal(&self.database())
            ),
        )?;
        Ok(!rows.is_empty())
    }

    fn provision(&self, credentials: &Credentials) -> Result<(), Error> {
        self.admin(None, &format!(
            "CREATE USER IF NOT EXISTS {}@'%' IDENTIFIED BY {}; GRANT ALL PRIVILEGES ON `{}`.* TO {}@'%'",
            literal(&credentials.user), literal(&credentials.password),
            credentials.database, literal(&credentials.user)
        ))?;
        Ok(())
    }

    fn canonical_remote(&self) -> Result<(), Error> {
        let database = self.database();
        let remote = self.remote_url();
        let directory = self.data.join("native-remote");
        check_directory(&directory)?;
        fs::create_dir_all(&directory)?;
        let rows = self.admin(
            Some(&database),
            "SELECT name, url FROM dolt_remotes WHERE name = 'origin'",
        )?;
        if rows
            .first()
            .and_then(|row| row.get("url"))
            .and_then(serde_json::Value::as_str)
            == Some(remote.as_str())
        {
            return Ok(());
        }
        if !rows.is_empty() {
            self.admin(Some(&database), "CALL DOLT_REMOTE('remove', 'origin')")?;
        }
        self.admin(
            Some(&database),
            &format!("CALL DOLT_REMOTE('add', 'origin', {})", literal(&remote)),
        )?;
        Ok(())
    }

    fn read_record(&self) -> Result<Option<Record>, Error> {
        let path = self.root.join("registration.json");
        let file = match File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::metadata(&self.root)?.uid()
            || metadata.len() > 16_384
        {
            return Err(Error::UnsafePath);
        }
        let mut bytes = Vec::new();
        file.take(16_385).read_to_end(&mut bytes)?;
        if bytes.len() > 16_384 {
            return Err(Error::UnsafePath);
        }
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.version != 1
            || record.namespace != crate::control::Namespace::from(&self.namespace)
            || record.branch != self.branch
            || record.credentials.database != self.database()
            || record.credentials.user != self.user()
            || record.credentials.password.len() != 64
            || !record
                .credentials
                .password
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Incomplete);
        }
        Ok(Some(record))
    }

    fn write_record(&self, record: &Record) -> Result<(), Error> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(&serde_json::to_vec(record)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join("registration.json"))
            .map_err(|error| error.error)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn admin(&self, database: Option<&str>, query: &str) -> Result<Vec<serde_json::Value>, Error> {
        self.query(None, database, query)
    }

    fn query(
        &self,
        credentials: Option<&Credentials>,
        database: Option<&str>,
        query: &str,
    ) -> Result<Vec<serde_json::Value>, Error> {
        let mut command = self.executor.command(credentials, database);
        command.current_dir(&self.root);
        let mut child = command.spawn()?;
        let write = child
            .stdin
            .take()
            .ok_or(Error::Result)?
            .write_all(query.as_bytes());
        let output = child.wait_with_output()?;
        write?;
        if !output.status.success() {
            // SQL may contain a credential; never return native stdout or the statement itself.
            return Err(Error::Native {
                stderr: if credentials.is_some() {
                    "scoped authentication/query failed".into()
                } else {
                    "management query failed; inspect the retained registration phase".into()
                },
            });
        }
        let mut rows = Vec::new();
        for value in
            serde_json::Deserializer::from_slice(&output.stdout).into_iter::<serde_json::Value>()
        {
            let value = value?;
            if let Some(result) = value.get("rows").and_then(serde_json::Value::as_array) {
                rows.extend(result.iter().cloned());
            }
        }
        Ok(rows)
    }
}

impl Executor {
    fn command(&self, credentials: Option<&Credentials>, database: Option<&str>) -> Command {
        let mut command = Command::new("timeout");
        command.args(["--signal=KILL", "30"]);
        match self {
            Self::Tcp { port, password } => {
                command
                    .arg("dolt")
                    .args([
                        "--host",
                        "127.0.0.1",
                        "--port",
                        &port.to_string(),
                        "--no-tls",
                        "--password",
                        credentials.map_or(password.as_str(), Credentials::password),
                    ])
                    .args(["--user", credentials.map_or("root", Credentials::user)])
                    .env(
                        "DOLT_CLI_PASSWORD",
                        credentials.map_or(password.as_str(), Credentials::password),
                    );
            }
            Self::Container {
                runtime,
                container,
                data,
            } => {
                command.arg(runtime).args(["exec", "-i", container]);
                if let Some(credentials) = credentials {
                    command.args([
                        "env",
                        &format!("DOLT_CLI_PASSWORD={}", credentials.password()),
                        "dolt",
                        "--host",
                        "127.0.0.1",
                        "--port",
                        "3306",
                        "--no-tls",
                        "--password",
                        credentials.password(),
                        "--user",
                        credentials.user(),
                    ]);
                } else {
                    command.args(["sh", "-c", "export DOLT_CLI_PASSWORD=\"$(cat -- \"$1/admin.password\")\"; shift; exec dolt --host 127.0.0.1 --port 3306 --no-tls --password \"$DOLT_CLI_PASSWORD\" --user wrixadmin \"$@\"", "wrix-admin"])
                        .arg(data);
                }
            }
        }
        if let Some(database) = database {
            command.args(["--use-db", database]);
        }
        command
            .args(["sql", "--result-format", "json"])
            .env_remove("DOLT_ROOT_HOST")
            .env_remove("DOLT_ROOT_PASSWORD")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let parent = path.parent().ok_or(Error::UnsafePath)?;
    private_directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(value)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn serialize_branch<S: serde::Serializer>(
    branch: &Branch,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(branch.as_str())
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

fn random_password() -> Result<String, Error> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes
        .into_iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0xf)]),
            ]
        })
        .collect())
}

pub(crate) fn check_directory(path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => (),
            Ok(_) => return Err(Error::UnsafePath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn private_directory(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(Error::UnsafePath);
    }
    check_directory(path)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if path.canonicalize()? != path || fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(Error::UnsafePath);
    }
    Ok(())
}

#[cfg(test)]
mod test;
