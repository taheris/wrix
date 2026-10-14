use std::{fs, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};

use displaydoc::Display;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::{
    navigation::{Navigator, Outcome},
    process::Runner,
    route::{Platform, Route, Target, registration},
};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// attention storage I/O failed: {0}
    Io(#[from] std::io::Error),
    /// attention database failed: {0}
    Sqlite(#[from] rusqlite::Error),
    /// attention serialization failed: {0}
    Json(#[from] serde_json::Error),
    /// --return requires Linux/niri
    UnsupportedReturn,
}

pub fn open(directory: &Path) -> Result<Connection, Error> {
    fs::create_dir_all(directory)?;
    let path = directory.join("attention.sqlite");
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(30))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS pending (target TEXT PRIMARY KEY, route TEXT NOT NULL, sequence INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS return_position (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), route TEXT NOT NULL);")?;
    Ok(db)
}

pub fn enqueue(directory: &Path, target: &Target, platform: Platform) -> Result<(), Error> {
    let mut db = open(directory)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(record) = registration(directory, target, platform) {
        tx.execute("INSERT OR REPLACE INTO pending VALUES (?, ?, (SELECT COALESCE(MAX(sequence), 0) + 1 FROM pending))",
            params![target.as_str(), serde_json::to_string(&record.identity)?])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn focus<R: Runner>(
    directory: &Path,
    platform: Platform,
    navigator: &Navigator<R>,
) -> Result<(), Error> {
    let mut db = open(directory)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let entries = tx
        .prepare("SELECT target, route FROM pending ORDER BY sequence DESC")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (target, identity) in entries {
        let record = match Target::try_from(target.clone()) {
            Ok(target) => registration(directory, &target, platform),
            Err(error) => {
                tracing::debug!(%error, "invalid pending target");
                None
            }
        };
        let matches = match serde_json::from_str::<serde_json::Value>(&identity) {
            Ok(identity) => record.as_ref().is_some_and(|r| r.identity == identity),
            Err(error) => {
                tracing::debug!(%error, "invalid pending route identity");
                false
            }
        };
        let (result, origin) = match record.filter(|_| matches) {
            Some(record) => {
                let origin = if platform == Platform::Linux {
                    navigator.capture_origin(directory)
                } else {
                    None
                };
                (navigator.navigate(&record.route, false), origin)
            }
            None => (Outcome::Stale, None),
        };
        if result != Outcome::Retry {
            tx.execute("DELETE FROM pending WHERE target = ?", [&target])?;
        }
        if result == Outcome::Success {
            if platform == Platform::Linux {
                tx.execute("DELETE FROM return_position", [])?;
                if let Some(origin) = origin {
                    tx.execute(
                        "INSERT INTO return_position VALUES (1, ?)",
                        [serde_json::to_string(&origin.saved())?],
                    )?;
                }
            }
            break;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn return_to_origin<R: Runner>(
    directory: &Path,
    platform: Platform,
    navigator: &Navigator<R>,
) -> Result<(), Error> {
    if platform == Platform::Darwin {
        return Err(Error::UnsupportedReturn);
    }
    let mut db = open(directory)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let text = tx
        .query_row(
            "SELECT route FROM return_position WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(text) = text {
        let route = serde_json::from_str(&text)
            .map_err(crate::route::Error::from)
            .and_then(|value| Route::parse(&value, platform));
        let result = match route {
            Ok(route) => navigator.navigate(&route, true),
            Err(error) => {
                tracing::debug!(%error, "stale saved origin");
                Outcome::Stale
            }
        };
        if result != Outcome::Retry {
            tx.execute("DELETE FROM return_position", [])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn is_focused<R: Runner>(
    directory: &Path,
    target: &Target,
    platform: Platform,
    navigator: &Navigator<R>,
) -> bool {
    registration(directory, target, platform).is_some_and(|r| navigator.is_focused(&r.route))
}
