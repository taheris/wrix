//! Host control documents and non-authoritative filesystem exports.

pub mod document;
pub(crate) mod scope;

pub fn is_host_root(state_root: &Path) -> Result<bool, Error> {
    Ok(Target::parse(&state_root.join("services.json"))?.is_some())
}

pub fn context_at(state_root: &Path) -> Result<Option<crate::context::Context>, Error> {
    Target::parse(&state_root.join("services.json"))?
        .map(|target| scope::context(&target))
        .transpose()
}

use crate::{
    host::Host,
    store::{Error, Store},
};
use document::{ClosureName, Owner, PendingName};
use rusqlite::{OptionalExtension, params};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use wrix_core::path::WorkspaceHash;

pub enum Source {
    Host(Option<Vec<u8>>),
    Observer,
}

struct Target {
    host: Host,
    key: WorkspaceHash,
    owner: Owner,
}

impl Target {
    fn parse(path: &Path) -> Result<Option<Self>, Error> {
        let host = match Host::discover() {
            Ok(host) => host,
            Err(crate::host::Error::Worker) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
        let pending = parent.file_name().is_some_and(|name| name == "pending");
        let closure = parent.file_name().is_some_and(|name| name == "gcroots");
        let directory = if pending || closure {
            parent.parent().ok_or(Error::Unsupported)?
        } else {
            parent
        };
        let observed = wrix_core::host_path::anchored(directory)?;
        let state = wrix_core::host_path::anchored(host.roots().state())?;
        if observed.parent() != Some(state.join("workspaces").as_path()) {
            if directory.parent().is_some_and(|parent| {
                parent.file_name().is_some_and(|name| name == "workspaces")
                    && parent
                        .parent()
                        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "wrix"))
            }) {
                return Err(Error::RootMismatch);
            }
            return Ok(None);
        }
        if observed.file_name() != directory.file_name() {
            return Err(Error::PrivateState);
        }
        let key = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::Unsupported)?;
        let key = WorkspaceHash::parse(key).map_err(|error| Error::Invalid(error.to_string()))?;
        let owner = if pending {
            Owner::Pending(PendingName::parse(name)?)
        } else if closure {
            Owner::Closure(ClosureName::parse(name)?)
        } else {
            match name {
                "services.json" => Owner::Services,
                "cache-status.json" => Owner::CacheStatus,
                "publish-roots.json" => Owner::Manifest,
                _ => return Ok(None),
            }
        };
        Ok(Some(Self { host, key, owner }))
    }
}

pub fn read_at(path: &Path) -> Result<Source, Error> {
    let Some(target) = Target::parse(path)? else {
        return Ok(Source::Observer);
    };
    let Some(store) = Store::inspect(&target.host)? else {
        if path.try_exists()? {
            return Err(Error::Uninitialized);
        }
        return Ok(Source::Host(None));
    };
    if !registered(&store, &target.key)?
        && target
            .host
            .roots()
            .workspace_state(&target.key)
            .try_exists()?
    {
        return Err(Error::ImportRequired(target.key));
    }
    let value: Option<Vec<u8>> = store
        .connection
        .query_row(
            "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
            params![target.key.as_str(), target.owner.tag()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(value) = &value {
        target.owner.parse(value, &target.key)?;
    }
    Ok(Source::Host(value))
}

pub fn write_at(path: &Path, value: &[u8]) -> Result<(), Error> {
    let Some(target) = Target::parse(path)? else {
        return Ok(());
    };
    target.owner.parse(value, &target.key)?;
    let store = Store::mutate(&target.host, false)?;
    require_registered(&store, &target.key)?;
    store.connection.execute(
        "INSERT INTO namespace_metadata VALUES (?1, ?2, ?3) ON CONFLICT(native_key, owner) DO UPDATE SET value = excluded.value",
        params![target.key.as_str(), target.owner.tag(), value]
    )?;
    Ok(())
}

pub fn ensure_at(path: &Path, value: &[u8]) -> Result<Source, Error> {
    let Some(target) = Target::parse(path)? else {
        return Ok(Source::Observer);
    };
    target.owner.parse(value, &target.key)?;
    let store = Store::mutate(&target.host, false)?;
    require_registered(&store, &target.key)?;
    let tag = target.owner.tag();
    store.connection.execute(
        "INSERT OR IGNORE INTO namespace_metadata VALUES (?1, ?2, ?3)",
        params![target.key.as_str(), tag, value],
    )?;
    let value: Vec<u8> = store.connection.query_row(
        "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
        params![target.key.as_str(), tag],
        |row| row.get(0),
    )?;
    target.owner.parse(&value, &target.key)?;
    Ok(Source::Host(Some(value)))
}

pub fn remove_at(path: &Path) -> Result<(), Error> {
    let Some(target) = Target::parse(path)? else {
        return Ok(());
    };
    if !matches!(target.owner, Owner::Pending(_) | Owner::Closure(_)) {
        return Err(Error::Unsupported);
    }
    let store = Store::mutate(&target.host, false)?;
    require_registered(&store, &target.key)?;
    store.connection.execute(
        "DELETE FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
        params![target.key.as_str(), target.owner.tag()],
    )?;
    Ok(())
}

pub enum Listing {
    Host(Vec<(PathBuf, Vec<u8>)>),
    Observer,
}

#[derive(Clone, Copy)]
enum Collection {
    Pending,
    Closure,
}

impl Collection {
    const fn directory(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Closure => "gcroots",
        }
    }
    const fn prefix(&self) -> &str {
        match self {
            Self::Pending => "pending/",
            Self::Closure => "cache-closure/",
        }
    }
    fn owner(self, name: &str) -> Result<Owner, document::ParseError> {
        match self {
            Self::Pending => Ok(Owner::Pending(PendingName::parse(name)?)),
            Self::Closure => Ok(Owner::Closure(ClosureName::parse(name)?)),
        }
    }
}

pub fn pending_at(directory: &Path) -> Result<Listing, Error> {
    listing_at(directory, Collection::Pending)
}
pub fn closures_at(directory: &Path) -> Result<Listing, Error> {
    listing_at(directory, Collection::Closure)
}

fn listing_at(directory: &Path, collection: Collection) -> Result<Listing, Error> {
    if directory
        .file_name()
        .is_none_or(|name| name != collection.directory())
    {
        return Err(Error::Unsupported);
    }
    let Some(target) = Target::parse(&directory.join("probe.json"))? else {
        return Ok(Listing::Observer);
    };
    let Some(store) = Store::inspect(&target.host)? else {
        if directory.try_exists()? {
            return Err(Error::Uninitialized);
        }
        return Ok(Listing::Host(Vec::new()));
    };
    if !registered(&store, &target.key)?
        && target
            .host
            .roots()
            .workspace_state(&target.key)
            .try_exists()?
    {
        return Err(Error::ImportRequired(target.key));
    }
    let mut query = store.connection.prepare("SELECT owner, value FROM namespace_metadata WHERE native_key = ?1 AND owner LIKE ?2 ORDER BY owner")?;
    let rows = query
        .query_map(
            params![target.key.as_str(), format!("{}%", collection.prefix())],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries = Vec::with_capacity(rows.len());
    for (tag, bytes) in rows {
        let name = tag
            .strip_prefix(collection.prefix())
            .ok_or(Error::Unsupported)?;
        collection.owner(name)?.parse(&bytes, &target.key)?;
        entries.push((directory.join(name), bytes));
    }
    Ok(Listing::Host(entries))
}

pub struct ServiceEntry {
    pub key: WorkspaceHash,
    pub bytes: Vec<u8>,
}

pub fn services_at(state_root: &Path) -> Result<Option<Vec<ServiceEntry>>, Error> {
    let Some(target) = Target::parse(&state_root.join("services.json"))? else {
        return Ok(None);
    };
    let Some(store) = Store::inspect(&target.host)? else {
        return Ok(Some(Vec::new()));
    };
    let mut query = store.connection.prepare("SELECT native_key, value FROM namespace_metadata WHERE owner = 'services' ORDER BY native_key")?;
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries = Vec::with_capacity(rows.len());
    for (key, bytes) in rows {
        let key = WorkspaceHash::parse(&key).map_err(|_| Error::Unsupported)?;
        require_registered(&store, &key)?;
        Owner::Services.parse(&bytes, &key)?;
        entries.push(ServiceEntry { key, bytes });
    }
    Ok(Some(entries))
}

pub(crate) fn registered(store: &Store, key: &WorkspaceHash) -> Result<bool, Error> {
    registration(&store.connection, key)
}

pub(crate) fn registration(
    connection: &rusqlite::Connection,
    key: &WorkspaceHash,
) -> Result<bool, Error> {
    let value: Option<Vec<u8>> = connection.query_row("SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = 'namespace/registered'", [key.as_str()], |row| row.get(0)).optional()?;
    match value {
        Some(value) if value == [1] => Ok(true),
        None => Ok(false),
        Some(_) => Err(Error::Unsupported),
    }
}

pub(crate) fn register(store: &Store, key: &WorkspaceHash) -> Result<(), Error> {
    store.connection.execute(
        "INSERT OR IGNORE INTO namespace_metadata VALUES (?1, 'namespace/registered', X'01')",
        [key.as_str()],
    )?;
    Ok(())
}

fn require_registered(store: &Store, key: &WorkspaceHash) -> Result<(), Error> {
    if !registered(store, key)? {
        return Err(Error::ImportRequired(key.clone()));
    }
    Ok(())
}

pub(crate) fn authorize(host: &Host, context: &crate::context::Context) -> Result<(), Error> {
    let store = Store::mutate(host, false)?;
    for directory in [
        host.roots().state().join("workspaces"),
        host.roots().workspace_state(context.key()),
    ] {
        match fs::symlink_metadata(&directory) {
            Ok(_) => native_directory(&directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    store.connection.execute_batch("BEGIN IMMEDIATE")?;
    context.check_store(host, Some(&store))?;
    let key = context.key();
    if !context.integrated() {
        store.standalone(key, !host.roots().workspace_state(key).try_exists()?)?;
    }
    if !registered(&store, key)? {
        if context.integrated() {
            return Err(Error::Unsupported);
        }
        if host.roots().workspace_state(key).try_exists()? {
            return Err(Error::ImportRequired(key.clone()));
        }
        register(&store, key)?;
    }
    if !context.integrated() {
        scope::observe(&store, context)?;
    }
    store.connection.execute_batch("COMMIT")?;
    Ok(())
}

pub(crate) fn import(host: &Host, store: &mut Store, key: &WorkspaceHash) -> Result<(), Error> {
    if registered(store, key)? {
        return Ok(());
    }
    let directory = host.roots().state().join("workspaces");
    native_directory(&directory)?;
    let root = host.roots().workspace_state(key);
    native_directory(&root)?;
    let mut files = ["services.json", "cache-status.json", "publish-roots.json"]
        .map(|name| root.join(name))
        .to_vec();
    for collection in ["pending", "gcroots"] {
        let directory = root.join(collection);
        match fs::symlink_metadata(&directory) {
            Ok(_) => {
                native_directory(&directory)?;
                for file in fs::read_dir(&directory)? {
                    files.push(file?.path());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    let tx = store.connection.transaction()?;
    for path in files {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
            || metadata.nlink() != 1
            || metadata.len() > 16 * 1024 * 1024
        {
            return Err(Error::PrivateState);
        }
        let target = Target::parse(&path)?.ok_or(Error::Unsupported)?;
        let mut bytes = fs::read(&path)?;
        if matches!(target.owner, Owner::Pending(_)) {
            let mut value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(document::ParseError::from)?;
            if value
                .get("created_at_epoch")
                .is_none_or(serde_json::Value::is_null)
            {
                let epoch = metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| Error::Invalid(error.to_string()))?
                    .as_secs();
                value["created_at_epoch"] = epoch.into();
                bytes = serde_json::to_vec(&value).map_err(document::ParseError::from)?;
            }
        }
        target.owner.parse(&bytes, key)?;
        tx.execute(
            "INSERT INTO namespace_metadata VALUES (?1, ?2, ?3)",
            params![key.as_str(), target.owner.tag(), bytes],
        )?;
    }
    tx.execute(
        "INSERT INTO namespace_metadata VALUES (?1, 'namespace/registered', X'01')",
        [key.as_str()],
    )?;
    tx.commit()?;
    Ok(())
}

fn native_directory(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::PrivateState);
    }
    Ok(())
}
