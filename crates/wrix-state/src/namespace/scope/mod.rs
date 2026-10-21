//! Saved paths are lookup hints; fresh host selection remains the authority.

use crate::{
    context::Context,
    store::{Error, Store},
};
use rusqlite::{OptionalExtension, params};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};
use wrix_core::path::WorkspaceHash;

const OWNER: &str = "namespace/scope-observation";

fn observation(store: &Store, key: &WorkspaceHash) -> Result<Option<PathBuf>, Error> {
    let bytes = store
        .connection
        .query_row(
            "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
            params![key.as_str(), OWNER],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    bytes.map(parse).transpose()
}

fn parse(bytes: Vec<u8>) -> Result<PathBuf, Error> {
    if bytes.contains(&0) {
        return Err(Error::Unsupported);
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Unsupported);
    }
    Ok(path)
}

pub fn check(store: &Store, key: &WorkspaceHash, scope: &Path) -> Result<(), Error> {
    if observation(store, key)?.is_some_and(|path| path.as_os_str() != scope.as_os_str()) {
        return Err(Error::Conflict);
    }
    Ok(())
}

pub fn observe(store: &Store, context: &Context) -> Result<(), Error> {
    check(store, context.key(), context.primary())?;
    store.connection.execute(
        "INSERT OR IGNORE INTO namespace_metadata VALUES (?1, ?2, ?3)",
        params![
            context.key().as_str(),
            OWNER,
            context.primary().as_os_str().as_bytes()
        ],
    )?;
    Ok(())
}

pub(super) fn context(target: &super::Target) -> Result<Context, Error> {
    let store = Store::inspect(&target.host)?.ok_or(Error::Uninitialized)?;
    super::require_registered(&store, &target.key)?;
    let binding = store
        .connection
        .query_row(
            "SELECT primary_root FROM binding WHERE native_key = ?1",
            [target.key.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(parse)
        .transpose()?;
    let path = binding
        .or(observation(&store, &target.key)?)
        .ok_or_else(|| Error::ScopeRequired(target.key.clone()))?;
    let context = Context::inspect(&target.host, &path)?;
    if context.key() != &target.key {
        return Err(Error::Conflict);
    }
    Ok(context)
}
