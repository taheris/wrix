use std::path::{Path, PathBuf};

use rusqlite::params;
use tracing::{debug, warn};

use crate::identifier::{MoleculeId, SpecLabel};

use super::companions::parse_companions;
use super::db::{StateDb, drop_and_recreate};
use super::error::StateError;

/// One active molecule from `bd list --status=open --label=ralph:active`.
///
/// `rebuild` consumes pre-fetched values rather than calling `bd` itself —
/// the caller (e.g. `loom init --rebuild` wiring `BdClient`) is responsible
/// for issuing the CLI calls. Keeps `loom-core` free of subprocess
/// orchestration and makes rebuild testable without a real `bd` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMolecule {
    pub id: MoleculeId,
    pub spec_label: SpecLabel,
    pub base_commit: Option<String>,
}

/// Counts of rows written by [`StateDb::rebuild`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildReport {
    pub specs: usize,
    pub molecules: usize,
    pub companions: usize,
}

impl StateDb {
    /// Drop all state-DB tables, recreate the schema, and repopulate from:
    ///
    /// 1. `<workspace>/specs/*.md` — one `specs` row per file (label = file
    ///    stem; path = repo-relative POSIX).
    /// 2. `molecules` argument — one `molecules` row per active molecule.
    /// 3. Each spec's `## Companions` section — one `companions` row per
    ///    listed path. Specs without the section contribute zero rows.
    ///
    /// Iteration counters reset to 0; implementation notes are lost (the only
    /// field with no external source of truth).
    pub fn rebuild(
        &self,
        workspace: &Path,
        molecules: &[ActiveMolecule],
    ) -> Result<RebuildReport, StateError> {
        let specs_dir = workspace.join("specs");
        let spec_files = collect_spec_files(&specs_dir)?;

        self.with_conn(|conn| {
            drop_and_recreate(conn)?;
            let mut report = RebuildReport::default();

            for (label, rel_path, content) in &spec_files {
                conn.execute(
                    "INSERT INTO specs(label, spec_path, implementation_notes)
                     VALUES (?1, ?2, NULL)",
                    params![label.as_str(), rel_path.to_string_lossy()],
                )?;
                report.specs += 1;

                for path in parse_companions(content) {
                    conn.execute(
                        "INSERT OR IGNORE INTO companions(spec_label, companion_path)
                         VALUES (?1, ?2)",
                        params![label.as_str(), path],
                    )?;
                    report.companions += 1;
                }
            }

            for mol in molecules {
                if !spec_files.iter().any(|(l, _, _)| l == &mol.spec_label) {
                    warn!(
                        molecule = %mol.id,
                        spec = %mol.spec_label,
                        "skipping molecule whose spec_label has no spec file",
                    );
                    continue;
                }
                conn.execute(
                    "INSERT INTO molecules(id, spec_label, base_commit, iteration_count)
                     VALUES (?1, ?2, ?3, 0)",
                    params![mol.id.as_str(), mol.spec_label.as_str(), mol.base_commit],
                )?;
                report.molecules += 1;
            }

            debug!(?report, "state-db rebuild complete");
            Ok(report)
        })
    }
}

fn collect_spec_files(specs_dir: &Path) -> Result<Vec<(SpecLabel, PathBuf, String)>, StateError> {
    if !specs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(specs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let content = std::fs::read_to_string(&path)?;
        let rel = PathBuf::from("specs").join(format!("{stem}.md"));
        out.push((SpecLabel::new(stem.to_string()), rel, content));
    }
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    Ok(out)
}
