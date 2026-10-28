//! Versioned private host authority; never mount this socket or its directory into guests.

mod assignment;
pub mod cache;
mod deployment;
mod endpoint;
pub mod gateway;
mod pool;
pub use pool::Status as PoolPreparation;
pub(crate) mod maintenance;
mod registration;
pub mod serving;
pub mod supervision;

use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    time::timeout,
};
use wrix_core::path::WorkspaceHash;

pub(crate) const VERSION: u16 = 2;
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
    /// persisted generation assignments are inconsistent with this deployment
    InvalidSelection,
    /// this controller has already issued its deployment role listeners
    EndpointsAlreadyBound,
    /// private control registry lock is poisoned
    LockPoisoned,
    /// persisted namespace registration has an unsupported version, duplicate identity or excess entries
    InvalidRegistration,
    /// controller registry requires maintenance: run `wrix service repair` with the updated host package to stop the verified old controller and upgrade it; never remove ownership or policy files
    RegistryUpgrade,
    /// updated host controller did not confirm compatible authenticated readiness
    ControllerReadiness,
    /// workspace namespace is not registered
    UnknownNamespace,
    /// workspace policy denies the requested permission
    PermissionDenied,
    /// host cache credential issuance is unavailable; inspect host authorization and retained issuer state
    CacheUnavailable,
    /// installed-store controller preparation interface is unavailable; start a compatible host controller
    PoolInterface,
    /// installed-store controller preparation request was rejected; inspect current approval and retained ownership
    PoolRequest,
    /// installed-store admission failed: {0}
    PoolAdmission(#[from] wrix_state::nix_store::Error),
    /// installed-store host preparation failed: {0}
    PoolStorage(#[from] crate::nix_store::owner::Error),
    /// installed-store native owner failed: {0}
    PoolOwner(#[from] crate::nix_store::owner::backend::Error),
    /// installed-store lifecycle admission failed: {0}
    PoolLifecycle(#[from] crate::nix_store::lifecycle::Error),
}

/// Inspect a retained deployment without creating authority, locks or listeners.
pub(crate) fn existing_deployment(
    host: &wrix_state::host::Host,
) -> Result<Option<crate::deployment::Id>, Error> {
    drop(wrix_state::store::Store::inspect(host).map_err(io::Error::other)?);
    let root = host.roots().state().join("control/authority");
    let metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || root.canonicalize()? != root
        || metadata.uid() != fs::metadata(host.roots().state())?.uid()
    {
        return Err(Error::PrivateRoot);
    }
    deployment::load(&root)
}

/// Existing namespace readiness does not admit or reactivate any workspace.
pub(crate) fn required_sql(state: &Path) -> Result<Vec<Namespace>, Error> {
    let root = state.join("control/authority");
    if !root.try_exists()? {
        let registrations = state.join("host-control/dolt");
        if registrations.try_exists()? && fs::read_dir(registrations)?.next().transpose()?.is_some()
        {
            return Err(Error::InvalidRegistration);
        }
        return Ok(Vec::new());
    }
    let registry = registration::Store::open(&root)?.lock()?;
    Ok(registry
        .iter()
        .filter(|(_, policy)| policy.dolt == Access::Allowed)
        .map(|(namespace, _)| namespace.clone())
        .collect())
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
enum Authority<'a> {
    Controller {
        _lease: registration::Lease,
    },
    Host {
        _host: wrix_state::host::Host,
        _selection: &'a wrix_state::context::Selection,
        _lease: registration::Lease,
        root: PathBuf,
    },
}

pub struct Authorization<'a> {
    authority: Authority<'a>,
    namespace: Namespace,
    permission: Permission,
}

impl<'a> Authorization<'a> {
    /// Check the durable controller registry, retaining policy exclusion through
    /// native issuance. A repository marker alone never authorizes SQL.
    pub fn host_sql(selection: &'a wrix_state::context::Selection) -> Result<Self, Error> {
        Self::host_sql_policy(selection, false)
    }

    /// Explicit host lifecycle admission, never passive client delivery.
    pub(crate) fn admit_host_sql(
        selection: &'a wrix_state::context::Selection,
    ) -> Result<Self, Error> {
        Self::host_sql_policy(selection, true)
    }

    fn host_sql_policy(
        selection: &'a wrix_state::context::Selection,
        admit: bool,
    ) -> Result<Self, Error> {
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        selection.authorize_effects().map_err(io::Error::other)?;
        if !selection
            .workspace()
            .canonical_path()
            .join(".beads/dolt")
            .is_dir()
        {
            return Err(Error::PermissionDenied);
        }
        let root = host.roots().state().join("control/authority");
        if admit {
            wrix_core::private_directory::prepare_with_parents(&root).map_err(io::Error::other)?;
        } else if !root.try_exists()? {
            return Err(Error::PermissionDenied);
        }
        let store = registration::Store::open(&root)?;
        let namespace = Namespace::from(selection.workspace().hash());
        let lease = store.lease(&namespace)?;
        let mut registry = store.lock()?;
        if admit {
            if !registry.contains_key(&namespace) && registry.len() >= MAX_NAMESPACES {
                return Err(Error::InvalidRegistration);
            }
            let policy = registry.entry(namespace.clone()).or_insert(Policy {
                dolt: Access::Denied,
                private_cache_read: Access::Denied,
                common_cache_read: Access::Denied,
                egress: Access::Denied,
                compiler: Access::Denied,
            });
            policy.dolt = Access::Allowed;
            registry.commit()?;
        }
        if registry
            .get(&namespace)
            .is_none_or(|policy| policy.dolt != Access::Allowed)
        {
            return Err(Error::PermissionDenied);
        }
        Ok(Self {
            authority: Authority::Host {
                _host: host,
                _selection: selection,
                _lease: lease,
                root,
            },
            namespace: Namespace::from(selection.workspace().hash()),
            permission: Permission::Sql,
        })
    }

    pub(crate) fn retire_sql(self) -> Result<(), Error> {
        let Authority::Host { root, .. } = &self.authority else {
            return Err(Error::PermissionDenied);
        };
        let mut registry = registration::Store::open(root)?.lock()?;
        let policy = registry
            .get_mut(&self.namespace)
            .ok_or(Error::UnknownNamespace)?;
        policy.dolt = Access::Denied;
        registry.commit()
    }

    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    pub const fn permission(&self) -> Permission {
        self.permission
    }
}

/// Explicit host maintenance can upgrade a stopped controller's policy epoch.
/// A live old controller is never replaced or allowed to overwrite new policy.
pub(crate) fn recover_registry(host: &wrix_state::host::Host) -> Result<(), Error> {
    let root = host.roots().state().join("control/authority");
    if !root.try_exists()? {
        return Ok(());
    }
    let store = registration::Store::open(&root)?;
    // A current live controller already has a v2 snapshot; no migration needed.
    match store.load() {
        Ok(_) => return Ok(()),
        Err(Error::RegistryUpgrade) => (),
        Err(error) => return Err(error),
    }
    let owner = registration::Ownership::acquire(&root).map_err(|error| match error {
        Error::Io(error) if error.kind() == io::ErrorKind::WouldBlock => Error::RegistryUpgrade,
        other => other,
    })?;
    store.initialize(&owner)
}

/// Host retirement shares the controller's registry lock, including while the
/// controller is stopped. Revoke only cache audiences, preserving SQL policy.
pub(crate) fn retire_host_cache(selection: &wrix_state::context::Selection) -> Result<(), Error> {
    let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
    selection.authorize_effects().map_err(io::Error::other)?;
    let root = host.roots().state().join("control/authority");
    if !root.try_exists()? {
        return Ok(());
    }
    let store = registration::Store::open(&root)?;
    let namespace = Namespace::from(selection.workspace().hash());
    let _lease = store.lease(&namespace)?;
    let mut registry = store.lock()?;
    if let Some(policy) = registry.get_mut(&namespace) {
        policy.private_cache_read = Access::Denied;
        policy.common_cache_read = Access::Denied;
        registry.commit()?;
    }
    Ok(())
}

/// Local issuance authority only; this is neither a serializable grant nor native pool readiness.
pub struct PoolAuthorization {
    _lease: registration::Lease,
    admission: wrix_state::nix_store::admission::Guard,
}

impl PoolAuthorization {
    pub const fn pool(&self) -> &wrix_core::nix_store::pool::Key {
        self.admission.pool()
    }

    pub const fn namespace(&self) -> &WorkspaceHash {
        self.admission.namespace()
    }

    pub fn recheck(&self) -> Result<(), Error> {
        Ok(self.admission.recheck()?)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Inspect {},
    InspectCore {},
    InspectNixPool {},
    InspectNixExecution {},
    InspectNixWorker {},
    InspectNixPodmanWorker {},
    InspectNixKrunWorker {},
    InspectNixAppleWorker {},
    PrepareNixWorker {
        id: wrix_state::nix_store::admission::request::Id,
    },
    PrepareNixExecution {
        id: wrix_state::nix_store::admission::request::Id,
    },
    InspectNixExecutionDelivery {
        id: wrix_state::nix_store::admission::request::Id,
    },
    PrepareNixPool {
        id: wrix_state::nix_store::admission::request::Id,
    },
    InspectNixPoolPreparation {
        id: wrix_state::nix_store::admission::request::Id,
    },
    Register {
        namespace: Namespace,
        policy: Policy,
    },
    Retire {
        namespace: Namespace,
    },
    AdmitCache {
        namespace: Namespace,
    },
    IssueCache {
        namespace: Namespace,
    },
}

/// Canonical workspace identity, not a path or authority-bearing workspace pointer.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Namespace(String);

impl Namespace {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    CoreInterface {
        version: u16,
        ports: [std::num::NonZeroU16; 5],
        guest: Option<std::net::Ipv4Addr>,
    },
    Snapshot {
        version: u16,
        namespaces: BTreeMap<Namespace, Policy>,
    },
    Applied,
    PoolInterface {
        version: u16,
    },
    PoolDelivery {
        delivery: crate::nix_store::owner::backend::client::Delivery,
    },
    PoolQueued {
        id: wrix_state::nix_store::admission::request::Id,
    },
    PoolPreparation {
        status: PoolPreparation,
    },
    CacheIssued {
        expires_at: u64,
    },
    Conflict,
    Capacity,
    Incompatible,
}

/// One descriptor owns the local control endpoint until dropped; it is close-on-exec.
pub struct Controller {
    listener: UnixListener,
    _ownership: registration::Ownership,
    credential: Credential,
    uid: u32,
    registrations: registration::Store,
    endpoints: endpoint::Store,
    deployment: crate::deployment::Id,
    assignments: assignment::Store,
    cache_issuer: Option<PathBuf>,
    pool_jobs: pool::Queue,
    pub(crate) gateway: tokio::sync::RwLock<Option<std::sync::Arc<gateway::Issuer>>>,
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
        let ownership = registration::Ownership::acquire(root)?;
        if deployment::load(root)?.is_some() && !root.join("namespaces.json").try_exists()? {
            return Err(Error::InvalidRegistration);
        }
        let registrations = registration::Store::open(root)?;
        registrations.initialize(&ownership)?;
        let endpoints = endpoint::Store::open(root)?;
        let recorded_deployment = deployment::load(root)?;
        let assignments = assignment::Store::open(root, recorded_deployment.as_ref())?;
        let deployment = match recorded_deployment {
            Some(id) => id,
            None => deployment::create(root)?,
        };
        let credential = Credential::load(root)?;
        let mut epoch = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut epoch)?;
        let mut marker = tempfile::NamedTempFile::new_in(root)?;
        marker.write_all(&epoch)?;
        marker.as_file().sync_all()?;
        marker
            .persist(root.join("native-epoch"))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        let listener = UnixListener::bind(root.join("control.sock"))?;
        fs::set_permissions(root.join("control.sock"), fs::Permissions::from_mode(0o600))?;
        let controller = Self {
            listener,
            _ownership: ownership,
            credential,
            uid: metadata.uid(),
            registrations,
            endpoints,
            deployment,
            assignments,
            cache_issuer: None,
            pool_jobs: pool::Queue::default(),
            gateway: tokio::sync::RwLock::new(None),
        };
        controller.pool_jobs.recover_startup(&controller);
        Ok(controller)
    }

    /// Release the endpoint during a clean supervised stop, retaining private authority on disk.
    pub fn close(self) -> Result<(), Error> {
        let address = self.listener.local_addr()?;
        let path = address.as_pathname().ok_or(Error::PrivateRoot)?;
        fs::remove_file(path)?;
        Ok(())
    }

    pub(crate) fn authority_root(&self) -> Result<PathBuf, Error> {
        self.listener
            .local_addr()?
            .as_pathname()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or(Error::PrivateRoot)
    }

    /// Stable host identity survives controller restarts and namespace retirement.
    pub const fn deployment_id(&self) -> &crate::deployment::Id {
        &self.deployment
    }

    /// Inspect recorded intent only; no native readiness or ownership release is inferred.
    pub fn recorded_selection(
        &self,
    ) -> Result<Option<crate::deployment::selection::Record>, Error> {
        self.assignments.inspect()
    }

    /// Persist initial role assignments before runtime creation; ordinary callers resume existing intent.
    /// This host-only operation neither admits traffic nor authorizes replacing backend owners.
    pub fn bootstrap_selection(
        &self,
        image: wrix_sandbox::image::ImageRef,
        digest: wrix_sandbox::image::Digest,
    ) -> Result<crate::deployment::selection::Record, Error> {
        self.assignments.bootstrap(&self.deployment, image, digest)
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
            if request.version != VERSION {
                return send(&mut stream, &Reply::Incompatible).await;
            }
            match request.operation {
                Operation::IssueCache { namespace } => {
                    self.serve_cache_credential(&mut stream, &namespace).await
                }
                operation => send(&mut stream, &self.apply(operation)?).await,
            }
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
        let lease = self.registrations.lease(namespace)?;
        let registry = self.registrations.lock()?;
        let policy = registry.get(namespace).ok_or(Error::UnknownNamespace)?;
        if policy.access(permission) == Access::Denied {
            return Err(Error::PermissionDenied);
        }
        Ok(Authorization {
            authority: Authority::Controller { _lease: lease },
            namespace: namespace.clone(),
            permission,
        })
    }

    /// Synchronous host issuers bind checked approval and registration independently of cache/tool flags.
    /// Image, runtime, launch and owner/export admission remain separate obligations.
    pub fn authorize_nix_pool(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
    ) -> Result<PoolAuthorization, Error> {
        let admission = wrix_state::nix_store::admission::Guard::acquire(selection, system)?;
        let lease = self
            .registrations
            .lease(&Namespace::from(admission.namespace()))?;
        let registry = self.registrations.lock()?;
        if !registry.contains_key(&Namespace::from(admission.namespace())) {
            return Err(Error::UnknownNamespace);
        }
        admission.recheck()?;
        Ok(PoolAuthorization {
            _lease: lease,
            admission,
        })
    }

    /// Reserve a fresh pool under checked approval; no VM, credentials or worker view is issued.
    pub fn reserve_nix_pool(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
    ) -> Result<crate::nix_store::owner::Reservation, Error> {
        let authorization = self.authorize_nix_pool(selection, system)?;
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        let expected = host.roots().state().join("control/authority/control.sock");
        if self.listener.local_addr()?.as_pathname() != Some(expected.as_path()) {
            return Err(Error::PrivateRoot);
        }
        let _admission = crate::nix_store::lifecycle::gate::shared(&host)?;
        authorization.recheck()?;
        let reservation = crate::nix_store::owner::Reservation::reserve(
            &host,
            &self.deployment,
            authorization.pool(),
        )?;
        authorization.recheck()?;
        Ok(reservation)
    }

    /// Provision or authenticate the recorded VM; this does not deliver worker credentials or views.
    pub fn ensure_nix_pool(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
    ) -> Result<crate::nix_store::owner::backend::Owner, Error> {
        use crate::nix_store::{
            image::Candidate,
            owner::backend::{self, tools::Tools},
        };
        let candidate = Candidate::from_env().map_err(backend::Error::from)?;
        let tools = Tools::from_env()?;
        let authorization = self.authorize_nix_pool(selection, system)?;
        let pool = authorization.pool().clone();
        drop(authorization);
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        let expected = host.roots().state().join("control/authority/control.sock");
        if self.listener.local_addr()?.as_pathname() != Some(expected.as_path()) {
            return Err(Error::PrivateRoot);
        }
        let _admission = crate::nix_store::lifecycle::gate::shared(&host)?;
        let root = host
            .roots()
            .state()
            .join("control/nix")
            .join(pool.repository.to_string())
            .join(pool.system.as_str());
        let owner = match fs::symlink_metadata(root) {
            Ok(_) => {
                let mut owner = backend::Owner::open(&host, &self.deployment, &pool)?;
                owner.ensure_ready()?;
                owner
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tools.check(system)?;
                self.reserve_nix_pool(selection, system)?
                    .prepare(&tools)?
                    .start(candidate, tools)?
            }
            Err(error) => return Err(error.into()),
        };
        let authorization = self.authorize_nix_pool(selection, system)?;
        if authorization.pool() != owner.pool() {
            return Err(Error::PermissionDenied);
        }
        Ok(owner)
    }

    fn apply(&self, operation: Operation) -> Result<Reply, Error> {
        match operation {
            Operation::InspectCore {} => {
                let guest = self.endpoints.guest_address()?;
                return Ok(self
                    .endpoints
                    .bound_ports()?
                    .map_or(Reply::Incompatible, |ports| Reply::CoreInterface {
                        version: 2,
                        ports,
                        guest,
                    }));
            }
            Operation::InspectNixAppleWorker {} => {
                let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
                crate::nix_store::lifecycle::gate::check_clear(&host)?;
                crate::nix_store::owner::backend::tools::Tools::from_env()?
                    .view_check(wrix_state::nix_store::admission::request::Plane::Apple)?;
                return Ok(Reply::PoolInterface { version: 1 });
            }
            Operation::InspectNixPool {}
            | Operation::InspectNixExecution {}
            | Operation::InspectNixWorker {}
            | Operation::InspectNixPodmanWorker {}
            | Operation::InspectNixKrunWorker {} => {
                let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
                crate::nix_store::lifecycle::gate::check_clear(&host)?;
                return Ok(Reply::PoolInterface { version: 1 });
            }
            Operation::PrepareNixPool { id } => {
                return self.pool_jobs.prepare(self, id, pool::Action::Pool);
            }
            Operation::PrepareNixExecution { id } => {
                return self.pool_jobs.prepare(self, id, pool::Action::Execution);
            }
            Operation::PrepareNixWorker { id } => {
                return self.pool_jobs.prepare(self, id, pool::Action::Worker);
            }
            Operation::InspectNixExecutionDelivery { id } => return self.pool_jobs.delivery(&id),
            Operation::InspectNixPoolPreparation { id } => return self.pool_jobs.inspect(&id),
            _ => (),
        }
        // Namespace lease precedes the short global metadata transaction.
        let _lease = match &operation {
            Operation::Register { namespace, .. }
            | Operation::Retire { namespace }
            | Operation::AdmitCache { namespace } => Some(self.registrations.lease(namespace)?),
            Operation::Inspect {}
            | Operation::InspectCore {}
            | Operation::IssueCache { .. }
            | Operation::InspectNixPool {}
            | Operation::PrepareNixPool { .. }
            | Operation::InspectNixPoolPreparation { .. }
            | Operation::InspectNixExecution {}
            | Operation::PrepareNixExecution { .. }
            | Operation::InspectNixExecutionDelivery { .. }
            | Operation::InspectNixWorker {}
            | Operation::PrepareNixWorker { .. }
            | Operation::InspectNixPodmanWorker {}
            | Operation::InspectNixKrunWorker {}
            | Operation::InspectNixAppleWorker {} => None,
        };
        let mut namespaces = self.registrations.lock()?;
        Ok(match operation {
            Operation::InspectCore {}
            | Operation::IssueCache { .. }
            | Operation::InspectNixPool {}
            | Operation::PrepareNixPool { .. }
            | Operation::InspectNixPoolPreparation { .. }
            | Operation::InspectNixExecution {}
            | Operation::PrepareNixExecution { .. }
            | Operation::InspectNixExecutionDelivery { .. }
            | Operation::InspectNixWorker {}
            | Operation::PrepareNixWorker { .. }
            | Operation::InspectNixPodmanWorker {}
            | Operation::InspectNixKrunWorker {}
            | Operation::InspectNixAppleWorker {} => return Err(Error::PermissionDenied),
            Operation::Inspect {} => Reply::Snapshot {
                version: VERSION,
                namespaces: (*namespaces).clone(),
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
            Operation::AdmitCache { namespace } => {
                if !namespaces.contains_key(&namespace) && namespaces.len() >= MAX_NAMESPACES {
                    return Ok(Reply::Capacity);
                }
                let mut updated = (*namespaces).clone();
                let policy = updated.entry(namespace).or_insert(Policy {
                    dolt: Access::Denied,
                    private_cache_read: Access::Denied,
                    common_cache_read: Access::Denied,
                    egress: Access::Denied,
                    compiler: Access::Denied,
                });
                policy.private_cache_read = Access::Allowed;
                policy.common_cache_read = Access::Allowed;
                self.registrations.commit(&updated)?;
                *namespaces = updated;
                Reply::Applied
            }
            Operation::Retire { namespace } => {
                // Metadata-only IPC cannot claim native SQL revocation.
                if namespaces
                    .get(&namespace)
                    .is_some_and(|policy| policy.dolt == Access::Allowed)
                {
                    return Err(Error::PermissionDenied);
                }
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
    pub(crate) fn check_native_host(&self, host: &wrix_state::host::Host) -> Result<(), Error> {
        if self.socket != host.roots().state().join("control/authority/control.sock") {
            return Err(Error::PrivateRoot);
        }
        Ok(())
    }
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

    /// Probe before publishing a one-use checked request. Neither the job ID nor success grants a worker view.
    pub async fn prepare_nix_pool(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        if self.socket != host.roots().state().join("control/authority/control.sock") {
            return Err(Error::PrivateRoot);
        }
        if self.request(Operation::InspectNixPool {}).await?
            != (Reply::PoolInterface { version: 1 })
        {
            return Err(Error::PoolInterface);
        }
        let selection = selection.clone();
        let request = tokio::task::spawn_blocking(move || {
            wrix_state::nix_store::admission::Guard::acquire(&selection, system)?.request()
        })
        .await
        .map_err(io::Error::other)??;
        match self
            .request(Operation::PrepareNixPool {
                id: request.id().clone(),
            })
            .await?
        {
            Reply::PoolQueued { id } => Ok(id),
            _ => Err(Error::PoolRequest),
        }
    }

    /// Issue only against a checked never-started execution reservation; no raw pool/path is accepted.
    pub async fn prepare_nix_execution(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        if self.socket != host.roots().state().join("control/authority/control.sock") {
            return Err(Error::PrivateRoot);
        }
        if self.request(Operation::InspectNixExecution {}).await?
            != (Reply::PoolInterface { version: 1 })
        {
            return Err(Error::PoolInterface);
        }
        let selection = selection.clone();
        let ticket = ticket.clone();
        let request = tokio::task::spawn_blocking(move || {
            wrix_state::nix_store::admission::Guard::acquire(&selection, system)?
                .request_execution(ticket)
        })
        .await
        .map_err(io::Error::other)??;
        match self
            .request(Operation::PrepareNixExecution {
                id: request.id().clone(),
            })
            .await?
        {
            Reply::PoolQueued { id } => Ok(id),
            _ => Err(Error::PoolRequest),
        }
    }

    /// Prepare the exact installed image before issuing execution-scoped clients/views; runtime start is a separate handoff.
    pub async fn prepare_nix_worker(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
        image: &wrix_core::nix_store::binding::Binding,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        self.prepare_worker_on(
            selection,
            system,
            ticket,
            image,
            wrix_state::nix_store::admission::request::Plane::Host,
        )
        .await
    }

    /// Prepare the checked execution's source inside Podman's native rootless mount namespace, never the host namespace.
    pub async fn prepare_nix_podman_worker(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
        image: &wrix_core::nix_store::binding::Binding,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        self.prepare_worker_on(
            selection,
            system,
            ticket,
            image,
            wrix_state::nix_store::admission::request::Plane::Podman,
        )
        .await
    }

    /// Freeze Apple worker authority separately from host and Podman projection authority.
    pub async fn prepare_nix_apple_worker(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
        image: &wrix_core::nix_store::binding::Binding,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        self.prepare_worker_on(
            selection,
            system,
            ticket,
            image,
            wrix_state::nix_store::admission::request::Plane::Apple,
        )
        .await
    }

    /// Freeze separate-VM worker preparation while retaining Podman's private source namespace.
    pub async fn prepare_nix_krun_worker(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
        image: &wrix_core::nix_store::binding::Binding,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        self.prepare_worker_on(
            selection,
            system,
            ticket,
            image,
            wrix_state::nix_store::admission::request::Plane::Krun,
        )
        .await
    }

    async fn prepare_worker_on(
        &self,
        selection: &wrix_state::context::Selection,
        system: wrix_core::nix_store::pool::System,
        ticket: &wrix_state::execution::Ticket,
        image: &wrix_core::nix_store::binding::Binding,
        plane: wrix_state::nix_store::admission::request::Plane,
    ) -> Result<wrix_state::nix_store::admission::request::Id, Error> {
        use wrix_state::nix_store::admission::request::Plane;
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        if self.socket != host.roots().state().join("control/authority/control.sock") {
            return Err(Error::PrivateRoot);
        }
        let probe = match plane {
            Plane::Host => Operation::InspectNixWorker {},
            Plane::Podman => Operation::InspectNixPodmanWorker {},
            Plane::Krun => Operation::InspectNixKrunWorker {},
            Plane::Apple => Operation::InspectNixAppleWorker {},
        };
        if self.request(probe).await? != (Reply::PoolInterface { version: 1 }) {
            return Err(Error::PoolInterface);
        }
        let selection = selection.clone();
        let ticket = ticket.clone();
        let image = image.clone();
        let request = tokio::task::spawn_blocking(move || {
            let guard = wrix_state::nix_store::admission::Guard::acquire(&selection, system)?;
            match plane {
                Plane::Host => guard.request_worker(ticket, image),
                Plane::Podman => guard.request_podman_worker(ticket, image),
                Plane::Krun => guard.request_krun_worker(ticket, image),
                Plane::Apple => guard.request_apple_worker(ticket, image),
            }
        })
        .await
        .map_err(io::Error::other)??;
        match self
            .request(Operation::PrepareNixWorker {
                id: request.id().clone(),
            })
            .await?
        {
            Reply::PoolQueued { id } => Ok(id),
            _ => Err(Error::PoolRequest),
        }
    }

    pub async fn request(&self, operation: Operation) -> Result<Reply, Error> {
        timeout(IO_BUDGET, async {
            self.exchange(operation).await.map(|(_, reply)| reply)
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn exchange(&self, operation: Operation) -> Result<(UnixStream, Reply), Error> {
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
        let reply = receive(&mut stream).await?;
        Ok((stream, reply))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn controller_uses_current_durable_sql_policy_and_cache_admission_preserves_it() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("authority");
        let controller = Controller::bind(&root).unwrap();
        let namespace = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64)).unwrap());
        assert!(matches!(
            controller.authorize(&namespace, Permission::Sql),
            Err(Error::UnknownNamespace)
        ));
        // The host lifecycle and IPC operate on exactly the same authoritative
        // registry, even when admission happens after controller startup.
        let store = registration::Store::open(&root).unwrap();
        let mut policy = Policy {
            dolt: Access::Allowed,
            private_cache_read: Access::Denied,
            common_cache_read: Access::Denied,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        {
            let mut registry = store.lock().unwrap();
            registry.insert(namespace.clone(), policy);
            registry.commit().unwrap();
        }
        drop(controller.authorize(&namespace, Permission::Sql).unwrap());
        assert_eq!(
            controller
                .apply(Operation::AdmitCache {
                    namespace: namespace.clone()
                })
                .unwrap(),
            Reply::Applied
        );
        policy.private_cache_read = Access::Allowed;
        policy.common_cache_read = Access::Allowed;
        assert_eq!(store.lock().unwrap().get(&namespace), Some(&policy));
        {
            let mut registry = store.lock().unwrap();
            registry.get_mut(&namespace).unwrap().dolt = Access::Denied;
            registry.commit().unwrap();
        }
        assert!(matches!(
            controller.authorize(&namespace, Permission::Sql),
            Err(Error::PermissionDenied)
        ));
        drop(
            controller
                .authorize(&namespace, Permission::PrivateCacheRead)
                .unwrap(),
        );
        controller.close().unwrap();
    }

    #[tokio::test]
    async fn sql_native_lease_does_not_block_peer_authorization_or_allow_metadata_only_retirement()
    {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("authority");
        let controller = Controller::bind(&root).unwrap();
        let first = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64)).unwrap());
        let peer = Namespace::from(&WorkspaceHash::parse(&"b".repeat(64)).unwrap());
        let policy = Policy {
            dolt: Access::Allowed,
            private_cache_read: Access::Allowed,
            common_cache_read: Access::Allowed,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        for namespace in [&first, &peer] {
            assert_eq!(
                controller
                    .apply(Operation::Register {
                        namespace: namespace.clone(),
                        policy
                    })
                    .unwrap(),
                Reply::Applied
            );
        }
        let native_lease = controller.authorize(&first, Permission::Sql).unwrap();
        drop(controller.authorize(&peer, Permission::Sql).unwrap());
        drop(
            controller
                .authorize(&peer, Permission::PrivateCacheRead)
                .unwrap(),
        );
        // A long native restore holds no global registry lock.
        drop(controller.registrations.lock().unwrap());
        assert!(
            controller
                .apply(Operation::Retire {
                    namespace: first.clone()
                })
                .is_err()
        );
        drop(native_lease);
        assert!(matches!(
            controller.apply(Operation::Retire {
                namespace: first.clone()
            }),
            Err(Error::PermissionDenied)
        ));
        drop(controller.authorize(&first, Permission::Sql).unwrap());
        controller.close().unwrap();
    }

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
