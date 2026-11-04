//! Explicit host associations retain former namespaces without inferring membership from paths.

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use wrix_core::path::WorkspaceHash;

use crate::{host::Host, resolver::Checkout};

use super::{Error, Store};

const HOST: &str = "wrix-host-association";
pub(super) const FORMER: &str = "namespace/associated-former";

type Binding = (String, String, Vec<u8>, Vec<u8>);
type AssociationRow = (String, String, Vec<u8>, Vec<u8>, Vec<u8>);

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Former {
    version: u8,
    destination: String,
    checkout: Vec<u8>,
    evidence: Vec<u8>,
}

pub(super) fn reserved(slot: &str) -> bool {
    slot == HOST
}

pub(super) fn check_former(
    db: &rusqlite::Connection,
    key: &str,
    bytes: &[u8],
) -> Result<(), Error> {
    let record: Former = serde_json::from_slice(bytes).map_err(|_| Error::Unsupported)?;
    let destination = WorkspaceHash::parse(&record.destination).map_err(|_| Error::Unsupported)?;
    let checkout = path(record.checkout)?;
    if record.version != 1
        || destination.as_str() == key
        || WorkspaceHash::from_path(&checkout).as_str() != key
    {
        return Err(Error::Unsupported);
    }
    crate::resolver::check_evidence(&record.evidence)?;
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM participant p JOIN binding b ON p.native_key = b.native_key WHERE p.checkout = ?1 AND p.native_key = ?2 AND p.evidence = ?3 AND b.slot = ?4)",
        params![checkout.as_os_str().as_bytes(), destination.as_str(), record.evidence, HOST],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(Error::Unsupported);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Association {
    key: WorkspaceHash,
    checkout: Checkout,
    primary: Checkout,
}

impl Association {
    pub const fn key(&self) -> &WorkspaceHash {
        &self.key
    }

    pub const fn primary(&self) -> &Checkout {
        &self.primary
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "namespace": self.key.as_str(),
            "checkout": self.checkout.snapshot(),
            "primary": self.primary.snapshot(),
        })
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.key == other.key
            && self.checkout.matches(&other.checkout)
            && self.primary.matches(&other.primary)
    }
}

fn path(bytes: Vec<u8>) -> Result<PathBuf, Error> {
    if bytes.contains(&0) {
        return Err(Error::Unsupported);
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    if !path.is_absolute()
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::Unsupported);
    }
    Ok(path)
}

fn participant(
    db: &rusqlite::Connection,
    checkout: &Checkout,
    key: &WorkspaceHash,
) -> Result<(), Error> {
    let previous: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT native_key, evidence FROM participant WHERE checkout = ?1",
            [checkout.root().as_os_str().as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if previous.as_ref().is_some_and(|(existing, evidence)| {
        existing != key.as_str() || evidence != checkout.evidence()
    }) {
        return Err(Error::Conflict);
    }
    Ok(())
}

fn binding(
    db: &rusqlite::Connection,
    key: &WorkspaceHash,
    slot: &str,
    membership: &str,
    primary: &Checkout,
) -> Result<(), Error> {
    let previous: Option<Binding> = db
        .query_row(
            "SELECT slot, membership, primary_root, primary_evidence FROM binding WHERE native_key = ?1",
            [key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(owner, member, root, evidence)| {
            owner != slot
                || member != membership
                || root != primary.root().as_os_str().as_bytes()
                || evidence != primary.evidence()
        })
    {
        return Err(Error::Conflict);
    }
    db.execute(
        "INSERT OR IGNORE INTO binding VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            key.as_str(),
            slot,
            membership,
            primary.root().as_os_str().as_bytes(),
            primary.evidence()
        ],
    )?;
    Ok(())
}

impl Store {
    /// Associate exact host checkouts with an existing primary namespace; former state is reserved, never merged or deleted.
    pub fn associate(
        host: &Host,
        primary: &Path,
        checkout: &Path,
        retain: &WorkspaceHash,
    ) -> Result<WorkspaceHash, Error> {
        host.check()?;
        if !primary.is_absolute() || !checkout.is_absolute() {
            return Err(Error::Invalid("association paths must be absolute".into()));
        }
        let primary = Checkout::discover(primary)?;
        let checkout = Checkout::discover(checkout)?;
        if WorkspaceHash::from_path(primary.root()) != *retain {
            return Err(Error::Invalid(
                "association must retain the primary checkout's existing standalone namespace"
                    .into(),
            ));
        }
        Self::check_retention(host, retain)?;
        let former = WorkspaceHash::from_path(checkout.root());
        let mut store = Self::mutate(host, false)?;
        if !store.initialized()? {
            return Err(Error::Uninitialized);
        }
        crate::namespace::scope::check(&store, retain, primary.root())?;
        if store.integration()?.is_some() {
            return Err(Error::Invalid(
                "host association requires disabled external resolution; retain the selected adapter and use its admission workflow".into(),
            ));
        }
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        primary.recheck()?;
        checkout.recheck()?;
        if !crate::namespace::registration(&tx, retain)? {
            return Err(Error::Unsupported);
        }
        participant(&tx, &primary, retain)?;
        participant(&tx, &checkout, retain)?;
        binding(&tx, retain, HOST, retain.as_str(), &primary)?;
        if former != *retain {
            let reserved: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM binding WHERE native_key = ?1)",
                [former.as_str()],
                |row| row.get(0),
            )?;
            if reserved {
                return Err(Error::Conflict);
            }
            let bytes = serde_json::to_vec(&Former {
                version: 1,
                destination: retain.as_str().to_owned(),
                checkout: checkout.root().as_os_str().as_bytes().to_vec(),
                evidence: checkout.evidence().to_vec(),
            })
            .map_err(|_| Error::Unsupported)?;
            let existing: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
                    params![former.as_str(), FORMER],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.as_ref().is_some_and(|previous| previous != &bytes) {
                return Err(Error::Conflict);
            }
            tx.execute(
                "INSERT OR IGNORE INTO namespace_metadata VALUES (?1, ?2, ?3)",
                params![former.as_str(), FORMER, bytes],
            )?;
        }
        for observed in [&primary, &checkout] {
            tx.execute(
                "INSERT OR IGNORE INTO participant VALUES (?1, ?2, ?3)",
                params![
                    observed.root().as_os_str().as_bytes(),
                    retain.as_str(),
                    observed.evidence()
                ],
            )?;
        }
        primary.recheck()?;
        checkout.recheck()?;
        tx.commit()?;
        Ok(former)
    }

    pub(crate) fn association(&self, checkout: &Checkout) -> Result<Option<Association>, Error> {
        let row: Option<AssociationRow> = self
            .connection
            .query_row(
                "SELECT b.native_key, b.membership, b.primary_root, b.primary_evidence, p.evidence FROM participant p JOIN binding b ON p.native_key = b.native_key WHERE p.checkout = ?1 AND b.slot = ?2",
                params![checkout.root().as_os_str().as_bytes(), HOST],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        let Some((key, membership, root, primary_evidence, evidence)) = row else {
            return Ok(None);
        };
        if !self.initialized()? || membership != key || evidence != checkout.evidence() {
            return Err(Error::Conflict);
        }
        let key = WorkspaceHash::parse(&key).map_err(|_| Error::Unsupported)?;
        if !crate::namespace::registered(self, &key)? {
            return Err(Error::Unsupported);
        }
        let root = path(root)?;
        let primary = Checkout::discover(&root)?;
        if primary.root() != root
            || primary.evidence() != primary_evidence
            || WorkspaceHash::from_path(primary.root()) != key
        {
            return Err(Error::Conflict);
        }
        let primary_member: Option<(String, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT native_key, evidence FROM participant WHERE checkout = ?1",
                [root.as_os_str().as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if primary_member.as_ref().is_none_or(|(member, evidence)| {
            member != key.as_str() || evidence != primary.evidence()
        }) {
            return Err(Error::Conflict);
        }
        let former = WorkspaceHash::from_path(checkout.root());
        if former != key {
            let bytes: Vec<u8> = self.connection.query_row(
                "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
                params![former.as_str(), FORMER],
                |row| row.get(0),
            )?;
            check_former(&self.connection, former.as_str(), &bytes)?;
            let retained: Former =
                serde_json::from_slice(&bytes).map_err(|_| Error::Unsupported)?;
            if retained.destination != key.as_str()
                || retained.checkout != checkout.root().as_os_str().as_bytes()
                || retained.evidence != checkout.evidence()
            {
                return Err(Error::Conflict);
            }
        }
        checkout.recheck()?;
        primary.recheck()?;
        Ok(Some(Association {
            key,
            checkout: checkout.clone(),
            primary,
        }))
    }
}
