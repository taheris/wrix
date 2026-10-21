//! Persisted observations have supported shapes before any ordinary operation can reuse them.

use super::Error;
use crate::{integration::Slot, resolver::Membership};
use rusqlite::Connection;
use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Component, PathBuf},
};
use wrix_core::path::WorkspaceHash;

fn path(bytes: Vec<u8>) -> Result<(), Error> {
    if bytes.contains(&0) {
        return Err(Error::Unsupported);
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    let normalized = path.components().collect::<PathBuf>();
    if !path.is_absolute()
        || path.as_os_str() != normalized.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}

pub(super) fn check(db: &Connection) -> Result<(), Error> {
    let generation: Vec<u8> = db.query_row(
        "SELECT generation FROM authority WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if generation.len() != 32 {
        return Err(Error::Unsupported);
    }
    let mut integrations = db.prepare("SELECT slot, executable FROM integration")?;
    for row in integrations.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
    })? {
        let (slot, executable) = row?;
        Slot::parse(&slot)?;
        if executable.contains(&0) || !PathBuf::from(OsString::from_vec(executable)).is_absolute() {
            return Err(Error::Unsupported);
        }
    }
    let incomplete: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM authority WHERE admission_initialized = 0 AND (EXISTS(SELECT 1 FROM integration) OR EXISTS(SELECT 1 FROM binding))) OR EXISTS(SELECT 1 FROM binding b WHERE NOT EXISTS(SELECT 1 FROM namespace_metadata n WHERE n.native_key = b.native_key AND n.owner = 'namespace/registered' AND n.value = X'01')) OR EXISTS(SELECT 1 FROM binding b WHERE NOT EXISTS(SELECT 1 FROM participant p WHERE p.checkout = b.primary_root AND p.native_key = b.native_key AND p.evidence = b.primary_evidence))", [], |row| row.get(0))?;
    if incomplete {
        return Err(Error::Unsupported);
    }
    let mut bindings = db.prepare(
        "SELECT native_key, slot, membership, primary_root, primary_evidence FROM binding",
    )?;
    for row in bindings.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, Vec<u8>>(4)?,
        ))
    })? {
        let (key, slot, membership, root, evidence) = row?;
        WorkspaceHash::parse(&key).map_err(|_| Error::Unsupported)?;
        Slot::parse(&slot)?;
        Membership::parse(&membership)?;
        path(root)?;
        crate::resolver::check_evidence(&evidence)?;
    }
    let mut participants = db.prepare("SELECT checkout, evidence FROM participant")?;
    for row in participants.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })? {
        let (checkout, evidence) = row?;
        path(checkout)?;
        crate::resolver::check_evidence(&evidence)?;
    }
    Ok(())
}
