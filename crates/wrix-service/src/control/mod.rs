//! Versioned private host authority; never mount this socket or its directory into guests.

mod deployment;
mod endpoint;
mod registration;
pub mod supervision;

use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    time::timeout,
};
use wrix_core::path::WorkspaceHash;

const VERSION: u16 = 1;
const MAX_FRAME: usize = 1024 * 1024;
const MAX_NAMESPACES: usize = 4096;
const IO_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// private control I/O failed: {0}
    Io(#[from] io::Error),
    /// private control encoding failed: {0}
    Encoding(#[from] serde_json::Error),
    /// private control directory is not canonical, owned and private
    PrivateRoot,
    /// private control authentication failed
    Unauthorized,
    /// private control request timed out
    Timeout,
    /// private control frame exceeds its limit
    FrameLimit,
    /// fixed-role transport failed: {0}
    Route(#[from] crate::route::Error),
    /// persisted deployment role endpoints are invalid
    InvalidEndpoints,
    /// persisted host deployment identity has an unsupported version
    InvalidDeployment,
    /// this controller has already issued its deployment role listeners
    EndpointsAlreadyBound,
    /// private control registry lock is poisoned
    LockPoisoned,
    /// persisted namespace registration has an unsupported version, duplicate identity or excess entries
    InvalidRegistration,
    /// workspace namespace is not registered
    UnknownNamespace,
    /// workspace policy denies the requested permission
    PermissionDenied,
}

/// An independent host-management credential, never a cache/proxy traffic token.
struct Credential([u8; 32]);

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl Credential {
    fn load(root: &Path) -> Result<Self, Error> {
        let path = root.join("management.key");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                let mut bytes = [0; 32];
                File::open("/dev/urandom")?.read_exact(&mut bytes)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                Ok(Self(bytes))
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Self::read(root),
            Err(error) => Err(error.into()),
        }
    }

    fn read(root: &Path) -> Result<Self, Error> {
        let path = root.join("management.key");
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::metadata(root)?.uid()
            || metadata.len() != 32
        {
            return Err(Error::PrivateRoot);
        }
        let mut bytes = [0; 32];
        File::open(path)?.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Allowed,
    Denied,
}

/// Closed permission set; disabled features never become enabled by role selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub dolt: Access,
    pub private_cache_read: Access,
    pub common_cache_read: Access,
    pub egress: Access,
    pub compiler: Access,
}

/// Issuer permissions distinguish the two cache audiences even on a shared listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    Sql,
    PrivateCacheRead,
    CommonCacheRead,
    Egress,
    Compiler,
}

impl Policy {
    const fn access(self, permission: Permission) -> Access {
        match permission {
            Permission::Sql => self.dolt,
            Permission::PrivateCacheRead => self.private_cache_read,
            Permission::CommonCacheRead => self.common_cache_read,
            Permission::Egress => self.egress,
            Permission::Compiler => self.compiler,
        }
    }
}

/// Host-only issuance guard; retirement cannot race issuance while this guard is held.
/// It is not a traffic credential and cannot be constructed from container reports.
pub struct Authorization<'a> {
    _registry: MutexGuard<'a, BTreeMap<Namespace, Policy>>,
    namespace: Namespace,
    permission: Permission,
}

impl Authorization<'_> {
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    pub const fn permission(&self) -> Permission {
        self.permission
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Inspect {},
    Register {
        namespace: Namespace,
        policy: Policy,
    },
    Retire {
        namespace: Namespace,
    },
}

/// Canonical workspace identity, not a path or authority-bearing workspace pointer.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Namespace(String);

impl TryFrom<String> for Namespace {
    type Error = wrix_core::path::WorkspaceHashParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Ok(Self(WorkspaceHash::parse(&value)?.as_str().to_owned()))
    }
}

impl From<Namespace> for String {
    fn from(value: Namespace) -> Self {
        value.0
    }
}

impl From<&WorkspaceHash> for Namespace {
    fn from(value: &WorkspaceHash) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u16,
    management: [u8; 32],
    operation: Operation,
}

/// Secret-free result; protocol rejection never conveys private filesystem coordinates.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Snapshot {
        version: u16,
        namespaces: BTreeMap<Namespace, Policy>,
    },
    Applied,
    Conflict,
    Capacity,
    Incompatible,
}

/// One descriptor owns the local control endpoint until dropped; it is close-on-exec.
pub struct Controller {
    listener: UnixListener,
    _ownership: File,
    credential: Credential,
    uid: u32,
    namespaces: Mutex<BTreeMap<Namespace, Policy>>,
    registrations: registration::Store,
    endpoints: endpoint::Store,
    deployment: crate::deployment::Id,
}

impl Controller {
    /// `root` is a dedicated host-selected authority directory, never project/service data.
    /// Existing socket names are not unlinked: recovery must reconcile the recorded owner.
    pub fn bind(root: &Path) -> Result<Self, Error> {
        let parent = root.parent().ok_or(Error::PrivateRoot)?;
        let name = root.file_name().ok_or(Error::PrivateRoot)?;
        if parent.canonicalize()?.join(name) != root {
            return Err(Error::PrivateRoot);
        }
        match DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || root.canonicalize()? != root {
            return Err(Error::PrivateRoot);
        }
        let ownership_path = root.join("ownership");
        match ownership_path.symlink_metadata() {
            Ok(metadata) if !metadata.is_file() => return Err(Error::PrivateRoot),
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(ownership_path)?;
        let owner = ownership.metadata()?;
        if owner.uid() != metadata.uid() || owner.mode() & 0o077 != 0 {
            return Err(Error::PrivateRoot);
        }
        ownership.try_lock_exclusive()?;
        let registrations = registration::Store::open(root)?;
        let namespaces = registrations.load()?;
        let endpoints = endpoint::Store::open(root)?;
        let deployment = match deployment::load(root)? {
            Some(id) => id,
            None => deployment::create(root)?,
        };
        let credential = Credential::load(root)?;
        let listener = UnixListener::bind(root.join("control.sock"))?;
        fs::set_permissions(root.join("control.sock"), fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            _ownership: ownership,
            credential,
            uid: metadata.uid(),
            namespaces: Mutex::new(namespaces),
            registrations,
            endpoints,
            deployment,
        })
    }

    /// Release the endpoint during a clean supervised stop, retaining private authority on disk.
    pub fn close(self) -> Result<(), Error> {
        let address = self.listener.local_addr()?;
        let path = address.as_pathname().ok_or(Error::PrivateRoot)?;
        fs::remove_file(path)?;
        Ok(())
    }

    /// Stable host identity survives controller restarts and namespace retirement.
    pub const fn deployment_id(&self) -> &crate::deployment::Id {
        &self.deployment
    }

    /// Allocate the deployment's complete loopback listener set once, independent of namespaces.
    /// Callers must retain the listeners in their supervised front door for this controller's lifetime.
    pub async fn bind_role_listeners(&self) -> Result<crate::route::Listeners, Error> {
        self.endpoints.bind().await
    }

    /// Accept one bounded authenticated request; callers retain supervision of the serving loop.
    pub async fn serve_one(&self) -> Result<(), Error> {
        let (mut stream, _) = self.listener.accept().await?;
        authorize_peer(stream.peer_cred()?.uid(), self.uid)?;
        timeout(IO_BUDGET, async {
            let request: Request = receive(&mut stream).await?;
            if !constant_time_equal(&request.management, &self.credential.0) {
                return Err(Error::Unauthorized);
            }
            let reply = if request.version == VERSION {
                self.apply(request.operation)?
            } else {
                Reply::Incompatible
            };
            send(&mut stream, &reply).await
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// Native host issuers must retain this guard until signing finishes; no keys cross control.
    pub fn authorize(
        &self,
        namespace: &Namespace,
        permission: Permission,
    ) -> Result<Authorization<'_>, Error> {
        let registry = self.namespaces.lock().map_err(|_| Error::LockPoisoned)?;
        let policy = registry.get(namespace).ok_or(Error::UnknownNamespace)?;
        if policy.access(permission) == Access::Denied {
            return Err(Error::PermissionDenied);
        }
        Ok(Authorization {
            _registry: registry,
            namespace: namespace.clone(),
            permission,
        })
    }

    fn apply(&self, operation: Operation) -> Result<Reply, Error> {
        let mut namespaces = self.namespaces.lock().map_err(|_| Error::LockPoisoned)?;
        Ok(match operation {
            Operation::Inspect {} => Reply::Snapshot {
                version: VERSION,
                namespaces: namespaces.clone(),
            },
            Operation::Register { namespace, policy } => {
                match namespaces.get(&namespace).copied() {
                    Some(existing) if existing == policy => Reply::Applied,
                    Some(_) => Reply::Conflict,
                    None if namespaces.len() == MAX_NAMESPACES => Reply::Capacity,
                    None => {
                        let mut updated = namespaces.clone();
                        updated.insert(namespace, policy);
                        self.registrations.commit(&updated)?;
                        *namespaces = updated;
                        Reply::Applied
                    }
                }
            }
            Operation::Retire { namespace } => {
                if namespaces.contains_key(&namespace) {
                    let mut updated = namespaces.clone();
                    updated.remove(&namespace);
                    self.registrations.commit(&updated)?;
                    *namespaces = updated;
                }
                Reply::Applied
            }
        })
    }
}

const fn authorize_peer(peer: u32, owner: u32) -> Result<(), Error> {
    if peer == owner {
        Ok(())
    } else {
        Err(Error::Unauthorized)
    }
}

fn constant_time_equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter()
        .zip(b)
        .fold(0, |difference, (a, b)| difference | (a ^ b))
        == 0
}

async fn receive<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T, Error> {
    let length = usize::try_from(stream.read_u32().await?).map_err(|_| Error::FrameLimit)?;
    if length > MAX_FRAME {
        return Err(Error::FrameLimit);
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn send(stream: &mut UnixStream, value: &(impl Serialize + Sync)) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(Error::FrameLimit);
    }
    stream
        .write_u32(u32::try_from(bytes.len()).map_err(|_| Error::FrameLimit)?)
        .await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

/// Private host client; no credential/path argument is accepted from the wire protocol.
pub struct Client {
    socket: PathBuf,
    credential: Credential,
    uid: u32,
}

impl Client {
    pub fn open(root: &Path) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || root.canonicalize()? != root {
            return Err(Error::PrivateRoot);
        }
        Ok(Self {
            socket: root.join("control.sock"),
            credential: Credential::read(root)?,
            uid: metadata.uid(),
        })
    }

    pub async fn request(&self, operation: Operation) -> Result<Reply, Error> {
        timeout(IO_BUDGET, async {
            let mut stream = UnixStream::connect(&self.socket).await?;
            authorize_peer(stream.peer_cred()?.uid(), self.uid)?;
            send(
                &mut stream,
                &Request {
                    version: VERSION,
                    management: self.credential.0,
                    operation,
                },
            )
            .await?;
            receive(&mut stream).await
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn peer_uid_is_required_even_with_a_valid_management_secret() {
        assert!(authorize_peer(1000, 1000).is_ok());
        assert!(matches!(
            authorize_peer(1001, 1000),
            Err(Error::Unauthorized)
        ));
        assert!(matches!(authorize_peer(0, 1000), Err(Error::Unauthorized)));
    }

    #[tokio::test]
    async fn kernel_peer_credentials_are_checked_before_request_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        let mut controller = Controller::bind(&root).unwrap();
        controller.uid = controller.uid.wrapping_add(1);
        let connecting = UnixStream::connect(root.join("control.sock"));
        let (served, stream) = tokio::join!(controller.serve_one(), connecting);
        assert!(stream.is_ok());
        assert!(matches!(served, Err(Error::Unauthorized)));
    }

    #[tokio::test]
    async fn namespace_capacity_is_bounded_without_losing_existing_policy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = Policy {
            dolt: Access::Allowed,
            private_cache_read: Access::Denied,
            common_cache_read: Access::Denied,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        let entries: Vec<_> = (0..MAX_NAMESPACES)
            .map(
                |index| serde_json::json!({"namespace": format!("{index:064x}"), "policy": policy}),
            )
            .collect();
        let path = root.join("namespaces.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"version": VERSION, "entries": entries}))
                .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&path).unwrap();
        let controller = Controller::bind(&root).unwrap();
        let namespace = Namespace::try_from(format!("{MAX_NAMESPACES:064x}")).unwrap();
        assert_eq!(
            controller
                .apply(Operation::Register { namespace, policy })
                .unwrap(),
            Reply::Capacity
        );
        let namespace = Namespace::try_from(format!("{:064x}", 0)).unwrap();
        assert_eq!(
            controller
                .apply(Operation::Register { namespace, policy })
                .unwrap(),
            Reply::Applied
        );
        let snapshot = controller.apply(Operation::Inspect {}).unwrap();
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= MAX_FRAME);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn ownership_descriptor_is_not_inherited_across_exec() {
        const DESCRIPTOR: &str = "WRIX_TEST_CONTROL_FD";
        const OWNER: &str = "WRIX_TEST_CONTROL_OWNER";
        if let Some(descriptor) = std::env::var_os(DESCRIPTOR) {
            let fd = Path::new("/proc/self/fd").join(descriptor);
            match fs::read_link(fd) {
                Ok(target) => assert_ne!(target, PathBuf::from(std::env::var_os(OWNER).unwrap())),
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        let _controller = Controller::bind(&root).unwrap();
        let descriptor = fs::read_dir("/proc/self/fd")
            .unwrap()
            .find_map(|entry| {
                let entry = entry.unwrap();
                match fs::read_link(entry.path()) {
                    Ok(target) if target == root.join("ownership") => Some(entry.file_name()),
                    Ok(_) => None,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => panic!("cannot inspect descriptor: {error}"),
                }
            })
            .unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control::test::ownership_descriptor_is_not_inherited_across_exec",
            ])
            .env(DESCRIPTOR, descriptor)
            .env(OWNER, root.join("ownership"))
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }

    #[tokio::test]
    async fn incomplete_frames_never_register_namespaces() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        let controller = Controller::bind(&root).unwrap();
        let exchange = async {
            let mut stream = UnixStream::connect(root.join("control.sock"))
                .await
                .unwrap();
            stream.write_u32(200).await.unwrap();
            stream.write_all(b"{").await.unwrap();
            stream.shutdown().await.unwrap();
        };
        let (served, ()) = tokio::join!(controller.serve_one(), exchange);
        assert!(matches!(served, Err(Error::Io(_))));
        assert_eq!(
            controller.apply(Operation::Inspect {}).unwrap(),
            Reply::Snapshot {
                version: VERSION,
                namespaces: BTreeMap::new()
            }
        );
    }
}
