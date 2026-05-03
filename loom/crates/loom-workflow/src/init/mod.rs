//! `loom init` — workspace bootstrap and optional state-DB rebuild.
//!
//! Acquires the workspace lock (errors immediately if any per-spec lock is
//! held), ensures `.wrapix/loom/config.toml` and `.wrapix/loom/state.db`
//! exist, and — when `--rebuild` is passed — drops/recreates the state DB
//! and repopulates it from `specs/*.md` plus a caller-supplied slice of
//! active molecules.
//!
//! Subprocess work (calling `bd list --status=open --label=ralph:active`
//! to enumerate active molecules) is split out into
//! [`fetch_active_molecules`] so the core init function stays sync and
//! unit-testable without a real `bd` binary.

mod error;

use std::fs;
use std::path::{Path, PathBuf};

use loom_core::bd::{BdClient, CommandRunner, ListOpts};
use loom_core::identifier::{MoleculeId, SpecLabel};
use loom_core::lock::LockManager;
use loom_core::state::{ActiveMolecule, RebuildReport, StateDb};

pub use error::InitError;

/// Default body for `.wrapix/loom/config.toml`. Mirrors the Configuration
/// section of `specs/loom-harness.md` verbatim so a fresh `loom init` writes
/// a file that round-trips through `LoomConfig::default()`.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default-config.toml");

const SPEC_LABEL_PREFIX: &str = "spec:";
const ACTIVE_LABEL: &str = "ralph:active";

/// Options accepted by [`run`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InitOpts {
    /// Drop and repopulate the state DB from on-disk specs + active beads.
    pub rebuild: bool,
}

/// Files touched by [`run`] and (optionally) the rebuild report.
#[derive(Debug, Clone)]
pub struct InitReport {
    pub config_path: PathBuf,
    pub state_db_path: PathBuf,
    pub config_created: bool,
    pub rebuild: Option<RebuildReport>,
}

/// Run `loom init` against `workspace`.
///
/// 1. Acquires the workspace lock — errors immediately with `WorkspaceBusy`
///    if any per-spec `<label>.lock` is held.
/// 2. Creates `<workspace>/.wrapix/loom/` and writes `config.toml` if it
///    does not already exist (existing config files are preserved).
/// 3. Opens `state.db` (creating the schema on first open). When
///    `opts.rebuild` is true, the file is dropped and recreated, and the
///    schema is repopulated from `specs/*.md` plus `molecules`.
pub fn run(
    workspace: &Path,
    opts: InitOpts,
    molecules: &[ActiveMolecule],
) -> Result<InitReport, InitError> {
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_workspace()?;

    let loom_dir = workspace.join(".wrapix/loom");
    fs::create_dir_all(&loom_dir).map_err(|source| InitError::CreateDir {
        path: loom_dir.clone(),
        source,
    })?;

    let config_path = loom_dir.join("config.toml");
    let state_db_path = loom_dir.join("state.db");

    let config_created = !config_path.exists();
    if config_created {
        fs::write(&config_path, DEFAULT_CONFIG_TOML).map_err(|source| InitError::WriteConfig {
            path: config_path.clone(),
            source,
        })?;
    }

    let rebuild_report = if opts.rebuild {
        let db = StateDb::recreate(&state_db_path)?;
        Some(db.rebuild(workspace, molecules)?)
    } else {
        let _db = StateDb::open(&state_db_path)?;
        None
    };

    Ok(InitReport {
        config_path,
        state_db_path,
        config_created,
        rebuild: rebuild_report,
    })
}

/// Enumerate active molecules via `bd list --status=open --label=ralph:active`.
/// Each returned bead's `spec:<label>` label resolves the [`SpecLabel`] for
/// the rebuilt row; beads without a `spec:` label produce
/// [`InitError::MissingSpecLabel`].
pub async fn fetch_active_molecules<R: CommandRunner>(
    bd: &BdClient<R>,
) -> Result<Vec<ActiveMolecule>, InitError> {
    let beads = bd
        .list(ListOpts {
            status: Some("open".into()),
            label: Some(ACTIVE_LABEL.into()),
        })
        .await?;
    let mut out = Vec::with_capacity(beads.len());
    for bead in beads {
        let spec_label = bead
            .labels
            .iter()
            .find_map(|l| l.strip_prefix(SPEC_LABEL_PREFIX))
            .ok_or_else(|| InitError::MissingSpecLabel {
                id: bead.id.to_string(),
            })?;
        out.push(ActiveMolecule {
            id: MoleculeId::new(bead.id.as_str()),
            spec_label: SpecLabel::new(spec_label.to_string()),
            base_commit: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_core::config::LoomConfig;
    use loom_core::lock::LockError;

    fn temp_workspace() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        // Sanity: the workspace must contain a `specs/` for rebuild to work,
        // but `run()` itself does not require it — empty rebuild is valid.
        Ok(dir)
    }

    #[test]
    fn run_creates_config_and_state_db() -> Result<()> {
        let dir = temp_workspace()?;
        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(report.config_created, "first init must write config");
        assert!(
            report.config_path.exists(),
            "config.toml must exist on disk"
        );
        assert!(report.state_db_path.exists(), "state.db must exist on disk");
        // The default body must round-trip into the typed config.
        let body = std::fs::read_to_string(&report.config_path)?;
        let parsed = LoomConfig::from_toml_str(&body)?;
        assert_eq!(parsed, LoomConfig::default());
        // No rebuild on a plain init.
        assert!(report.rebuild.is_none());
        Ok(())
    }

    #[test]
    fn run_preserves_existing_config_file() -> Result<()> {
        let dir = temp_workspace()?;
        let loom_dir = dir.path().join(".wrapix/loom");
        std::fs::create_dir_all(&loom_dir)?;
        let custom = "pinned_context = \"AGENTS.md\"\n";
        std::fs::write(loom_dir.join("config.toml"), custom)?;

        let report = run(dir.path(), InitOpts::default(), &[])?;
        assert!(!report.config_created);
        let body = std::fs::read_to_string(report.config_path)?;
        assert_eq!(body, custom, "existing config must not be overwritten");
        Ok(())
    }

    #[test]
    fn rebuild_drops_and_repopulates_state_db() -> Result<()> {
        let dir = temp_workspace()?;
        let specs = dir.path().join("specs");
        std::fs::create_dir_all(&specs)?;
        std::fs::write(specs.join("alpha.md"), "# alpha\n")?;
        std::fs::write(specs.join("beta.md"), "# beta\n")?;

        // First init seeds the DB and bumps an iteration so we can prove
        // rebuild wiped it.
        run(dir.path(), InitOpts::default(), &[])?;
        let molecules = vec![ActiveMolecule {
            id: MoleculeId::new("wx-mol.1"),
            spec_label: SpecLabel::new("alpha"),
            base_commit: None,
        }];
        let db = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
        db.rebuild(dir.path(), &molecules)?;
        let post = db.increment_iteration(&MoleculeId::new("wx-mol.1"))?;
        assert_eq!(post, 1);
        drop(db);

        let report = run(
            dir.path(),
            InitOpts { rebuild: true },
            &[ActiveMolecule {
                id: MoleculeId::new("wx-mol.1"),
                spec_label: SpecLabel::new("alpha"),
                base_commit: None,
            }],
        )?;
        let rb = report
            .rebuild
            .ok_or_else(|| anyhow::anyhow!("rebuild must produce a report"))?;
        assert_eq!(rb.specs, 2, "two spec files");
        assert_eq!(rb.molecules, 1, "one active molecule");

        // Iteration counter reset to 0 after rebuild.
        let db = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
        let row = db
            .active_molecule(&SpecLabel::new("alpha"))?
            .ok_or_else(|| anyhow::anyhow!("active molecule must exist"))?;
        assert_eq!(row.iteration_count, 0);
        Ok(())
    }

    #[test]
    fn workspace_lock_errors_when_spec_lock_held() -> Result<()> {
        let dir = temp_workspace()?;
        // Hold a per-spec lock by acquiring through a separate manager.
        let mgr = LockManager::new(dir.path())?;
        let _spec_guard = mgr.acquire_spec(&SpecLabel::new("alpha"))?;
        match run(dir.path(), InitOpts::default(), &[]) {
            Err(InitError::Lock(LockError::WorkspaceBusy { label })) => {
                assert_eq!(label, "alpha");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected WorkspaceBusy, got {other:?}")),
        }
    }
}
