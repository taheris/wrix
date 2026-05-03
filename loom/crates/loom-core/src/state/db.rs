use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::identifier::{MoleculeId, SpecLabel};

use super::error::StateError;

const SCHEMA_VERSION: &str = "1";

const MIGRATION_V1: &str = "
CREATE TABLE IF NOT EXISTS specs (
    label                TEXT PRIMARY KEY,
    spec_path            TEXT NOT NULL,
    implementation_notes TEXT
);
CREATE TABLE IF NOT EXISTS molecules (
    id              TEXT PRIMARY KEY,
    spec_label      TEXT NOT NULL REFERENCES specs(label),
    base_commit     TEXT,
    iteration_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const DROP_AND_RECREATE: &str = "
DROP TABLE IF EXISTS companions;
DROP TABLE IF EXISTS molecules;
DROP TABLE IF EXISTS specs;
DROP TABLE IF EXISTS meta;
";

/// Owned handle to the SQLite state database. Wraps the connection in a
/// `Mutex` so the type is `Send + Sync`; the underlying `rusqlite::Connection`
/// is `!Sync`.
pub struct StateDb {
    conn: Mutex<Connection>,
}

/// One row of the `specs` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRow {
    pub label: SpecLabel,
    pub spec_path: PathBuf,
    pub implementation_notes: Option<Vec<String>>,
}

/// One row of the `molecules` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoleculeRow {
    pub id: MoleculeId,
    pub spec_label: SpecLabel,
    pub base_commit: Option<String>,
    pub iteration_count: u32,
}

impl StateDb {
    /// Open or create a state DB at `path`, applying schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|source| StateError::OpenDb {
            path: path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(MIGRATION_V1)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Delete the file at `path` (if any) and re-open with a fresh schema.
    /// Used by `loom init --rebuild` to recover from a corrupted DB file.
    pub fn recreate(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Self::open(path)
    }

    /// Look up a single spec row by label.
    pub fn spec(&self, label: &SpecLabel) -> Result<SpecRow, StateError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT label, spec_path, implementation_notes FROM specs WHERE label = ?1",
            params![label.as_str()],
            row_to_spec,
        )
        .optional()?
        .ok_or_else(|| StateError::SpecNotFound {
            label: label.to_string(),
        })?
    }

    /// Most recent active molecule for the given spec, or `None` if there is
    /// no active molecule. Resolves ties with `ORDER BY id ASC`.
    pub fn active_molecule(&self, label: &SpecLabel) -> Result<Option<MoleculeRow>, StateError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, spec_label, base_commit, iteration_count
             FROM molecules WHERE spec_label = ?1 ORDER BY id ASC LIMIT 1",
            params![label.as_str()],
            row_to_molecule,
        )
        .optional()?
        .transpose()
    }

    /// Read the `current_spec` meta row.
    pub fn current_spec(&self) -> Result<Option<SpecLabel>, StateError> {
        let conn = self.lock_conn()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'current_spec'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.map(SpecLabel::new))
    }

    /// Write the `current_spec` meta row.
    pub fn set_current_spec(&self, label: &SpecLabel) -> Result<(), StateError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('current_spec', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![label.as_str()],
        )?;
        Ok(())
    }

    /// Increment the iteration counter for `mol_id` and return the new value.
    pub fn increment_iteration(&self, mol_id: &MoleculeId) -> Result<u32, StateError> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE molecules SET iteration_count = iteration_count + 1 WHERE id = ?1",
            params![mol_id.as_str()],
        )?;
        if updated == 0 {
            return Err(StateError::SpecNotFound {
                label: mol_id.to_string(),
            });
        }
        let count: i64 = conn.query_row(
            "SELECT iteration_count FROM molecules WHERE id = ?1",
            params![mol_id.as_str()],
            |r| r.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Borrow the underlying connection for code inside `state/` only.
    pub(super) fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, StateError>,
    ) -> Result<R, StateError> {
        let conn = self.lock_conn()?;
        f(&conn)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StateError> {
        self.conn.lock().map_err(|_| StateError::Poisoned)
    }
}

pub(super) fn drop_and_recreate(conn: &Connection) -> Result<(), StateError> {
    conn.execute_batch(DROP_AND_RECREATE)?;
    conn.execute_batch(MIGRATION_V1)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION],
    )?;
    Ok(())
}

fn row_to_spec(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<SpecRow, StateError>> {
    let label: String = row.get(0)?;
    let spec_path: String = row.get(1)?;
    let notes_raw: Option<String> = row.get(2)?;
    let notes = match notes_raw {
        None => None,
        Some(s) => match serde_json::from_str::<Vec<String>>(&s) {
            Ok(v) => Some(v),
            Err(source) => {
                return Ok(Err(StateError::Json {
                    column: "implementation_notes",
                    source,
                }));
            }
        },
    };
    Ok(Ok(SpecRow {
        label: SpecLabel::new(label),
        spec_path: PathBuf::from(spec_path),
        implementation_notes: notes,
    }))
}

fn row_to_molecule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<MoleculeRow, StateError>> {
    let id: String = row.get(0)?;
    let spec_label: String = row.get(1)?;
    let base_commit: Option<String> = row.get(2)?;
    let iteration_count: i64 = row.get(3)?;
    Ok(Ok(MoleculeRow {
        id: MoleculeId::new(id),
        spec_label: SpecLabel::new(spec_label),
        base_commit,
        iteration_count: iteration_count.max(0) as u32,
    }))
}
