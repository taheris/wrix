//! `loom logs` — locate the most recent per-bead NDJSON log on disk.
//!
//! Read-only, no lock acquired (per the lock matrix in
//! `specs/loom-harness.md`). Walks `<workspace>/.wrapix/loom/logs/` for
//! `*.ndjson` files and returns the path with the largest mtime; with
//! `--bead <id>` set, only files whose stem starts with `<id>-` are
//! considered.
//!
//! The selection is split out from any I/O so the binary can use the path
//! to stream contents (e.g. via `tail -f`) without this module pulling in
//! tokio just to print bytes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use displaydoc::Display;
use thiserror::Error;

use loom_core::identifier::BeadId;

const LOG_EXTENSION: &str = "ndjson";

/// Options for [`select_log`].
#[derive(Debug, Clone, Default)]
pub struct LogsOpts<'a> {
    /// Restrict the search to files belonging to this bead. When `None`,
    /// the most recent log across every bead in every spec is returned.
    pub bead: Option<&'a BeadId>,
}

/// Failures raised by [`select_log`].
#[derive(Debug, Display, Error)]
pub enum LogsError {
    /// io failure while walking logs directory
    Io(#[from] std::io::Error),

    /// no logs found under {root}
    NoLogs { root: PathBuf },

    /// no logs found for bead {bead} under {root}
    NoLogsForBead { bead: String, root: PathBuf },
}

/// Walk `logs_root` (typically `<workspace>/.wrapix/loom/logs/`) and return
/// the most recent `*.ndjson` log. The traversal is two levels deep —
/// `<root>/<spec-label>/<bead-id>-<utc>.ndjson` per the path layout in
/// `specs/loom-harness.md` *Run UX & Logging*.
pub fn select_log(logs_root: &Path, opts: LogsOpts<'_>) -> Result<PathBuf, LogsError> {
    let bead_filter = opts.bead.map(|b| b.as_str().to_string());
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    if !logs_root.exists() {
        return missing(logs_root, opts.bead);
    }
    for spec_entry in std::fs::read_dir(logs_root)? {
        let spec_entry = spec_entry?;
        let spec_path = spec_entry.path();
        if !spec_path.is_dir() {
            continue;
        }
        for bead_entry in std::fs::read_dir(&spec_path)? {
            let bead_entry = bead_entry?;
            let path = bead_entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some(LOG_EXTENSION) {
                continue;
            }
            if let Some(prefix) = &bead_filter {
                if !file_stem_belongs_to(&path, prefix) {
                    continue;
                }
            }
            let mtime = bead_entry
                .metadata()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push((mtime, path));
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    match candidates.into_iter().next() {
        Some((_, path)) => Ok(path),
        None => missing(logs_root, opts.bead),
    }
}

fn file_stem_belongs_to(path: &Path, bead: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    // Stems look like `<bead>-<utc>` per `bead_log_path`. Match the prefix
    // exactly so `wx-1` does not also match `wx-10`.
    stem == bead || stem.starts_with(&format!("{bead}-"))
}

fn missing(root: &Path, bead: Option<&BeadId>) -> Result<PathBuf, LogsError> {
    match bead {
        Some(b) => Err(LogsError::NoLogsForBead {
            bead: b.to_string(),
            root: root.to_path_buf(),
        }),
        None => Err(LogsError::NoLogs {
            root: root.to_path_buf(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::time::Duration;

    fn touch(path: &Path, mtime: SystemTime) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, b"event\n")?;
        let f = std::fs::File::options().write(true).open(path)?;
        f.set_modified(mtime)?;
        Ok(())
    }

    #[test]
    fn empty_root_returns_no_logs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let err = select_log(&dir.path().join(".wrapix/loom/logs"), LogsOpts::default())
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogs { .. }));
        Ok(())
    }

    #[test]
    fn returns_most_recent_log_across_specs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".wrapix/loom/logs");
        let now = SystemTime::now();
        let older = now - Duration::from_secs(120);
        touch(&root.join("alpha/wx-1-old.ndjson"), older)?;
        touch(&root.join("beta/wx-2-newer.ndjson"), now)?;
        let path = select_log(&root, LogsOpts::default())?;
        assert!(path.ends_with("wx-2-newer.ndjson"), "{path:?}");
        Ok(())
    }

    #[test]
    fn bead_filter_matches_prefix_exactly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".wrapix/loom/logs");
        let now = SystemTime::now();
        // wx-1 and wx-10 are distinct beads — the filter must not collapse
        // them.
        touch(&root.join("alpha/wx-10-newer.ndjson"), now)?;
        touch(
            &root.join("alpha/wx-1-older.ndjson"),
            now - Duration::from_secs(60),
        )?;
        let path = select_log(
            &root,
            LogsOpts {
                bead: Some(&BeadId::new("wx-1")),
            },
        )?;
        assert!(path.ends_with("wx-1-older.ndjson"), "{path:?}");
        Ok(())
    }

    #[test]
    fn missing_bead_filter_returns_typed_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".wrapix/loom/logs");
        touch(&root.join("alpha/wx-1-x.ndjson"), SystemTime::now())?;
        let err = select_log(
            &root,
            LogsOpts {
                bead: Some(&BeadId::new("wx-2")),
            },
        )
        .err()
        .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogsForBead { .. }));
        Ok(())
    }

    #[test]
    fn ignores_non_ndjson_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".wrapix/loom/logs");
        touch(&root.join("alpha/wx-1-x.txt"), SystemTime::now())?;
        let err = select_log(&root, LogsOpts::default())
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogs { .. }));
        Ok(())
    }
}
