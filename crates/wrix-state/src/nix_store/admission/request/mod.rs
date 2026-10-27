//! One-use checked infrastructure requests; neither identifiers nor results admit workers.

use super::Guard;
use crate::nix_store::{
    Error,
    record::{Catalog, Common},
};
use crate::{host::Host, store::Store};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use wrix_core::{nix_store::pool::Key, path::WorkspaceHash};

const PREFIX: &str = "installed-store-preparation:";
const LIMIT: i64 = 128;

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl std::fmt::Debug for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[PREPARATION REQUEST]")
    }
}

impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::PreparationRequest);
        }
        Ok(Self(value))
    }
}
impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    #[default]
    Host,
    Podman,
    Krun,
    Apple,
}
impl Plane {
    pub const fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    pub const fn is_podman_namespace(self) -> bool {
        matches!(self, Self::Podman | Self::Krun)
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum Purpose {
    #[default]
    Preparation,
    Execution {
        ticket: crate::execution::Ticket,
    },
    Worker {
        ticket: crate::execution::Ticket,
        image: wrix_core::nix_store::binding::Binding,
        #[serde(default, skip_serializing_if = "Plane::is_host")]
        plane: Plane,
    },
}

impl Purpose {
    const fn is_preparation(&self) -> bool {
        matches!(self, Self::Preparation)
    }
    fn recheck(&self, store: &Store, context: &crate::context::Context) -> Result<(), Error> {
        if let Self::Execution { ticket } | Self::Worker { ticket, .. } = self {
            crate::execution::require_reserved(store, context, ticket)?;
        }
        if let Self::Worker { image, .. } = self {
            image
                .metadata()
                .version
                .require_supported()
                .map_err(|_| Error::Unsupported)?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    generation: Vec<u8>,
    pool: Key,
    common: Common,
    original: crate::context::snapshot::Snapshot,
    #[serde(default, skip_serializing_if = "Purpose::is_preparation")]
    purpose: Purpose,
}

/// Host-private, single-use preparation intent, not a native grant or execution lease.
pub struct Request {
    id: Id,
}
impl Request {
    pub const fn id(&self) -> &Id {
        &self.id
    }
}

/// Consumed host authorization for one fixed pool, not a ready owner or client capability.
pub struct Accepted {
    host: std::sync::Arc<Host>,
    namespace: WorkspaceHash,
    pool: Key,
    generation: crate::store::Generation,
    common: Common,
    context: crate::context::Context,
    purpose: Purpose,
}
impl Accepted {
    pub fn host(&self) -> &Host {
        &self.host
    }
    pub fn host_handle(&self) -> std::sync::Arc<Host> {
        self.host.clone()
    }
    pub const fn namespace(&self) -> &WorkspaceHash {
        &self.namespace
    }
    pub const fn pool(&self) -> &Key {
        &self.pool
    }
    pub const fn purpose(&self) -> &Purpose {
        &self.purpose
    }
    pub const fn generation(&self) -> &crate::store::Generation {
        &self.generation
    }
    pub fn recheck(&self) -> Result<(), Error> {
        self.host.check()?;
        self.context.recheck(&self.host)?;
        let store = Store::inspect(&self.host)?.ok_or(Error::PreparationRequest)?;
        if store.generation()? != self.generation {
            return Err(Error::PreparationRequest);
        }
        self.purpose.recheck(&store, &self.context)?;
        let catalog = Catalog::load(&store)?;
        let record = catalog
            .0
            .iter()
            .find(|(key, _)| key == &self.namespace)
            .map(|(_, record)| record)
            .ok_or(Error::ApprovalRequired)?;
        if !record.enabled
            || record.repository != self.pool.repository
            || record.common != self.common
        {
            return Err(Error::ApprovalRequired);
        }
        self.common.recheck()
    }
}

impl Guard {
    /// Publish while original scope and approval are still checked; release database exclusion before I/O.
    pub fn request(self) -> Result<Request, Error> {
        self.issue(Purpose::Preparation)
    }

    pub fn request_execution(self, ticket: crate::execution::Ticket) -> Result<Request, Error> {
        self.issue(Purpose::Execution { ticket })
    }

    pub fn request_worker(
        self,
        ticket: crate::execution::Ticket,
        image: wrix_core::nix_store::binding::Binding,
    ) -> Result<Request, Error> {
        self.issue(Purpose::Worker {
            ticket,
            image,
            plane: Plane::Host,
        })
    }

    pub fn request_podman_worker(
        self,
        ticket: crate::execution::Ticket,
        image: wrix_core::nix_store::binding::Binding,
    ) -> Result<Request, Error> {
        self.issue(Purpose::Worker {
            ticket,
            image,
            plane: Plane::Podman,
        })
    }

    pub fn request_krun_worker(
        self,
        ticket: crate::execution::Ticket,
        image: wrix_core::nix_store::binding::Binding,
    ) -> Result<Request, Error> {
        self.issue(Purpose::Worker {
            ticket,
            image,
            plane: Plane::Krun,
        })
    }

    pub fn request_apple_worker(
        self,
        ticket: crate::execution::Ticket,
        image: wrix_core::nix_store::binding::Binding,
    ) -> Result<Request, Error> {
        self.issue(Purpose::Worker {
            ticket,
            image,
            plane: Plane::Apple,
        })
    }

    fn issue(self, purpose: Purpose) -> Result<Request, Error> {
        self.recheck()?;
        purpose.recheck(&self.store, &self.context)?;
        let count: i64 = self.store.connection.query_row(
            "SELECT count(*) FROM namespace_metadata WHERE native_key = ?1 AND owner LIKE 'installed-store-preparation:%'",
            [self.namespace().as_str()], |row| row.get(0),
        )?;
        if count >= LIMIT {
            return Err(Error::PreparationCapacity);
        }
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)?;
        let id = Id::try_from(format!("{:032x}", u128::from_be_bytes(bytes)))?;
        let owner = format!("{PREFIX}{}", id.0);
        let duplicate: bool = self.store.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM namespace_metadata WHERE owner = ?1)",
            [&owner],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(Error::IdentityCollision);
        }
        let generation: Vec<u8> = self.store.connection.query_row(
            "SELECT generation FROM authority WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if generation.len() != 32 {
            return Err(Error::Unsupported);
        }
        let record = Record {
            version: 1,
            generation,
            pool: self.pool.clone(),
            common: self.common.clone(),
            original: crate::context::snapshot::Snapshot::capture(&self.context),
            purpose,
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() > 65536 {
            return Err(Error::Unsupported);
        }
        self.store.connection.execute(
            "INSERT INTO namespace_metadata (native_key, owner, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![self.namespace().as_str(), owner, bytes],
        )?;
        self.recheck()?;
        self.store.connection.execute_batch("COMMIT")?;
        Ok(Request { id })
    }
}

/// Claim from native host authority only; stale approval, scope or database generation fails before effects.
pub fn claim(id: &Id) -> Result<Accepted, Error> {
    let host = Host::discover()?;
    let owner = format!("{PREFIX}{}", id.0);
    let store = Store::inspect(&host)?.ok_or(Error::PreparationRequest)?;
    let (namespace, bytes) = pending(&store, &owner)?;
    drop(store);
    let record: Record = serde_json::from_slice(&bytes)?;
    // Resolver I/O uses only the frozen original observation, outside the host writer window.
    let context = record.original.recheck(&host)?;
    if context.key().as_str() != namespace || Common::observe(&context)? != record.common {
        return Err(Error::PreparationRequest);
    }
    let store = Store::mutate(&host, false)?;
    store.connection.execute_batch("BEGIN IMMEDIATE")?;
    if pending(&store, &owner)? != (namespace.clone(), bytes) {
        return Err(Error::PreparationRequest);
    }
    context.check_store(&host, Some(&store))?;
    let namespace = WorkspaceHash::parse(&namespace).map_err(|_| Error::PreparationRequest)?;
    let generation: Vec<u8> = store.connection.query_row(
        "SELECT generation FROM authority WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if record.version != 1 || record.generation.len() != 32 || record.generation != generation {
        return Err(Error::PreparationRequest);
    }
    let catalog = Catalog::load(&store)?;
    let approved = catalog
        .0
        .iter()
        .find(|(key, _)| key == &namespace)
        .map(|(_, record)| record)
        .ok_or(Error::ApprovalRequired)?;
    if !approved.enabled
        || approved.repository != record.pool.repository
        || approved.common != record.common
    {
        return Err(Error::ApprovalRequired);
    }
    record.common.recheck()?;
    record.purpose.recheck(&store, &context)?;
    if store.connection.execute(
        "DELETE FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
        rusqlite::params![namespace.as_str(), owner],
    )? != 1
    {
        return Err(Error::PreparationRequest);
    }
    store.connection.execute_batch("COMMIT")?;
    let generation = store.generation()?;
    drop(store);
    Ok(Accepted {
        host: std::sync::Arc::new(host),
        namespace,
        pool: record.pool,
        generation,
        common: record.common,
        context,
        purpose: record.purpose,
    })
}

fn pending(store: &Store, owner: &str) -> Result<(String, Vec<u8>), Error> {
    let count: i64 = store.connection.query_row(
        "SELECT count(*) FROM namespace_metadata WHERE owner = ?1",
        [&owner],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(Error::PreparationRequest);
    }
    let row: Option<(String, Vec<u8>)> = store.connection.query_row(
        "SELECT native_key, value FROM namespace_metadata WHERE owner = ?1 AND length(value) BETWEEN 1 AND 65536", [&owner],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    row.ok_or(Error::PreparationRequest)
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn apple_authority_is_distinct_from_host_and_rootless_podman_namespace() {
        let plane: Plane = serde_json::from_str("\"apple\"").unwrap();
        assert_eq!(plane, Plane::Apple);
        assert!(!plane.is_host());
        assert!(!plane.is_podman_namespace());
        assert_eq!(serde_json::to_string(&plane).unwrap(), "\"apple\"");
        assert!(serde_json::from_str::<Plane>("\"apple_gateway\"").is_err());
    }
    #[test]
    fn preparation_identifiers_cannot_choose_a_namespace_path_or_command() {
        assert!(Id::try_from("a".repeat(32)).is_ok());
        for value in [
            "../scope".to_owned(),
            "A".repeat(32),
            "a".repeat(31),
            format!("{}\n", "a".repeat(32)),
        ] {
            assert!(Id::try_from(value).is_err());
        }
    }
}
