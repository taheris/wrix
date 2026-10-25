//! Explicit native-key assignment with transactional forward, reverse and participant constraints.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};
use wrix_core::{
    cache_key::CachePublicKey,
    path::{Workspace, WorkspaceHash},
};

use crate::{
    host::Host,
    resolver::{Membership, Resolution, resolve},
    store::{Error, Store},
};

pub enum Intent {
    Allocate,
    Retain(WorkspaceHash),
}

impl Store {
    pub fn admit(
        host: &Host,
        slot: &crate::integration::Slot,
        expected: &Membership,
        caller: &Path,
        intent: &Intent,
    ) -> Result<WorkspaceHash, Error> {
        let selected = Self::inspect(host)?.ok_or(Error::Uninitialized)?;
        if !selected.initialized()? {
            return Err(Error::Uninitialized);
        }
        let selection = selected
            .integration()?
            .ok_or_else(|| Error::Invalid("integration is disabled".into()))?;
        if &selection.0 != slot {
            return Err(Error::Conflict);
        }
        let resolution = resolve(host, &selection.1, caller)?;
        if resolution.membership() != expected {
            return Err(Error::Conflict);
        }
        let generation = selected.generation()?;
        drop(selected);
        let mut store = Self::mutate(host, false)?;
        if store.generation()? != generation
            || !store.initialized()?
            || store.integration()?.as_ref() != Some(&selection)
        {
            return Err(Error::Conflict);
        }
        // The host writer guard serializes mutations; unchanged admissions need only a read transaction.
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        resolution.recheck()?;
        let existing: Option<(String, Vec<u8>, Vec<u8>)> = tx
            .query_row(
                "SELECT native_key, primary_root, primary_evidence FROM binding WHERE slot = ?1 AND membership = ?2",
                params![slot.as_str(), expected.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let already_registered = existing.is_some();
        let (key, binding_changed) = if let Some((existing, primary, evidence)) = existing {
            let key = WorkspaceHash::parse(&existing)
                .map_err(|error| Error::Invalid(error.to_string()))?;
            if !crate::namespace::registration(&tx, &key)? {
                return Err(Error::Unsupported);
            }
            if let Intent::Retain(candidate) = intent
                && candidate != &key
            {
                return Err(Error::Conflict);
            }
            (
                key,
                primary != resolution.primary().root().as_os_str().as_bytes()
                    || evidence != resolution.primary().evidence(),
            )
        } else {
            let key = match intent {
                Intent::Allocate => {
                    let key = Workspace::from_path(resolution.primary().root())?
                        .hash()
                        .clone();
                    let registered: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM namespace_metadata WHERE native_key = ?1)",
                        [key.as_str()],
                        |row| row.get(0),
                    )?;
                    if registered {
                        return Err(Error::ImportRequired(key));
                    }
                    ensure_empty(host, &key)?;
                    key
                }
                Intent::Retain(key) => {
                    let imported = crate::namespace::registration(&tx, key)?;
                    if !imported {
                        return Err(Error::Invalid(
                            "explicit native metadata import required before retention".into(),
                        ));
                    }
                    validate_native(host, key)?;
                    key.clone()
                }
            };
            (key, true)
        };
        let reverse: Option<(String, String)> = tx
            .query_row(
                "SELECT slot, membership FROM binding WHERE native_key = ?1",
                [key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if reverse
            .as_ref()
            .is_some_and(|(owner, member)| owner != slot.as_str() || member != expected.as_str())
        {
            return Err(Error::Conflict);
        }
        let mut updated_participants = Vec::new();
        for participant in [resolution.checkout(), resolution.primary()] {
            let previous: Option<(String, Vec<u8>)> = tx
                .query_row(
                    "SELECT native_key, evidence FROM participant WHERE checkout = ?1",
                    [participant.root().as_os_str().as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if previous
                .as_ref()
                .is_some_and(|(previous, _)| previous != key.as_str())
            {
                return Err(Error::Conflict);
            }
            if previous
                .as_ref()
                .is_none_or(|(_, evidence)| evidence != participant.evidence())
            {
                updated_participants.push(participant);
            }
            let derived = Workspace::from_path(participant.root())?.hash().clone();
            if derived != key {
                let reserved: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM binding WHERE native_key = ?1)",
                    [derived.as_str()],
                    |row| row.get(0),
                )?;
                if reserved {
                    return Err(Error::Conflict);
                }
                ensure_empty(host, &derived)?;
            }
        }
        verify_candidates(host, &tx, &key, &resolution)?;
        if !already_registered {
            tx.execute(
                "INSERT OR IGNORE INTO namespace_metadata VALUES (?1, 'namespace/registered', X'01')",
                [key.as_str()],
            )?;
        }
        if binding_changed {
            tx.execute(
                "INSERT INTO binding VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(native_key) DO UPDATE SET primary_root = excluded.primary_root, primary_evidence = excluded.primary_evidence",
                params![key.as_str(), slot.as_str(), expected.as_str(), resolution.primary().root().as_os_str().as_bytes(), resolution.primary().evidence()],
            )?;
        }
        for participant in updated_participants {
            tx.execute("INSERT INTO participant VALUES (?1, ?2, ?3) ON CONFLICT(checkout) DO UPDATE SET evidence = excluded.evidence",
                params![participant.root().as_os_str().as_bytes(), key.as_str(), participant.evidence()])?;
        }
        resolution.recheck()?;
        tx.commit()?;
        Ok(key)
    }

    pub fn admitted(
        &self,
        host: &Host,
        slot: &crate::integration::Slot,
        resolution: &Resolution,
    ) -> Result<WorkspaceHash, Error> {
        let row: Option<(String, Vec<u8>, Vec<u8>)> = self.connection.query_row(
            "SELECT native_key, primary_root, primary_evidence FROM binding WHERE slot = ?1 AND membership = ?2",
            params![slot.as_str(), resolution.membership().as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).optional()?;
        let (key, primary, primary_evidence) =
            row.ok_or_else(|| Error::Invalid("explicit workspace admission required".into()))?;
        if primary != resolution.primary().root().as_os_str().as_bytes()
            || primary_evidence != resolution.primary().evidence()
        {
            return Err(Error::Invalid(
                "primary scope changed; explicit readmission required".into(),
            ));
        }
        for participant in [resolution.checkout(), resolution.primary()] {
            let evidence: Option<Vec<u8>> = self
                .connection
                .query_row(
                    "SELECT evidence FROM participant WHERE checkout = ?1 AND native_key = ?2",
                    params![participant.root().as_os_str().as_bytes(), key],
                    |row| row.get(0),
                )
                .optional()?;
            if evidence.as_deref() != Some(participant.evidence()) {
                return Err(Error::Invalid(
                    "participant unknown or changed; explicit readmission required".into(),
                ));
            }
        }
        resolution.recheck()?;
        let parsed =
            WorkspaceHash::parse(&key).map_err(|error| Error::Invalid(error.to_string()))?;
        verify_candidates(host, &self.connection, &parsed, resolution)?;
        WorkspaceHash::parse(&key).map_err(|error| Error::Invalid(error.to_string()))
    }
}

fn verify_candidates(
    host: &Host,
    db: &rusqlite::Connection,
    key: &WorkspaceHash,
    resolution: &Resolution,
) -> Result<(), Error> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    let mut candidates = vec![
        resolution.checkout().root().to_path_buf(),
        resolution.primary().root().to_path_buf(),
    ];
    let mut query = db.prepare("SELECT checkout FROM participant WHERE native_key = ?1")?;
    for path in query.query_map([key.as_str()], |row| row.get::<_, Vec<u8>>(0))? {
        let path = path?;
        if path.contains(&0) {
            return Err(Error::Unsupported);
        }
        let path = PathBuf::from(OsString::from_vec(path));
        if !path.is_absolute() {
            return Err(Error::Unsupported);
        }
        candidates.push(path);
    }
    for path in candidates {
        let derived = WorkspaceHash::from_path(&path);
        if &derived != key {
            let claimed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM binding WHERE native_key = ?1) OR EXISTS(SELECT 1 FROM namespace_metadata WHERE native_key = ?1)", [derived.as_str()], |row| row.get(0))?;
            if claimed {
                return Err(Error::Conflict);
            }
            ensure_empty(host, &derived)?;
        }
    }
    Ok(())
}

fn ensure_empty(host: &Host, key: &WorkspaceHash) -> Result<(), Error> {
    for path in [
        host.roots().workspace_state(key),
        host.roots().workspace_cache(key),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "existing native state requires explicit retention: {key}"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_native(host: &Host, key: &WorkspaceHash) -> Result<(), Error> {
    let root = host.roots().workspace_state(key);
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::PrivateState);
    }
    let service = crate::namespace::read_at(&root.join("services.json"))?;
    let (mut evidence, requires_keys) = match service {
        crate::namespace::Source::Host(Some(bytes)) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(crate::namespace::document::ParseError::from)?;
            let cache = value
                .get("endpoints")
                .and_then(|value| value.get("cache_http"));
            (true, cache.is_some_and(|value| !value.is_null()))
        }
        crate::namespace::Source::Host(None) => (false, false),
        crate::namespace::Source::Observer => return Err(Error::Unsupported),
    };
    let public = root.join("keys/cache.pub");
    match fs::symlink_metadata(&public) {
        Ok(public_metadata) => {
            let directory = fs::symlink_metadata(root.join("keys"))?;
            let secret = fs::symlink_metadata(root.join("keys/cache.secret"))?;
            if !directory.is_dir()
                || directory.uid() != rustix::process::geteuid().as_raw()
                || directory.mode() & 0o022 != 0
                || !secret.is_file()
                || !public_metadata.is_file()
                || secret.nlink() != 1
                || public_metadata.nlink() != 1
                || secret.mode() & 0o077 != 0
                || secret.uid() != rustix::process::geteuid().as_raw()
                || public_metadata.uid() != rustix::process::geteuid().as_raw()
                || public_metadata.mode() & 0o022 != 0
                || secret.len() > 4096
                || public_metadata.len() > 4096
            {
                return Err(Error::PrivateState);
            }
            let public = fs::read_to_string(public)?;
            CachePublicKey::parse(&public).map_err(|error| Error::Invalid(error.to_string()))?;
            let secret = fs::read_to_string(root.join("keys/cache.secret"))?;
            validate_keypair(&secret, &public)?;
            evidence = true;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !requires_keys => (),
        Err(error) => return Err(error.into()),
    }
    if !evidence {
        return Err(Error::Invalid(
            "existing native state is incomplete or unsupported".into(),
        ));
    }
    Ok(())
}

fn validate_keypair(secret: &str, public: &str) -> Result<(), Error> {
    use base64::{
        Engine as _,
        engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
    };
    let (secret_name, secret) = secret
        .trim_end()
        .split_once(':')
        .ok_or(Error::Unsupported)?;
    let (public_name, public) = public
        .trim_end()
        .split_once(':')
        .ok_or(Error::Unsupported)?;
    let decode = |value: &str| {
        if value.contains('=') {
            STANDARD.decode(value)
        } else {
            STANDARD_NO_PAD.decode(value)
        }
        .map_err(|_| Error::Unsupported)
    };
    let secret = decode(secret)?;
    let public = decode(public)?;
    if secret_name != public_name
        || secret.len() != 64
        || public.len() != 32
        || secret[32..] != public
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}
