use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::task::spawn_blocking;
use tokio::time::timeout;

use crate::identifier::{BeadId, SpecLabel};

use super::error::GitError;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const WORKTREE_BASE: &str = ".wrapix/worktree";
const BRANCH_PREFIX: &str = "loom";

/// Single typed surface for git operations.
///
/// Backend split is internal: `gix` handles read-only inspection (status,
/// diff, refs, commit graph, worktree iteration); the `git` CLI handles
/// worktree mutation and merge-back. Callers see only the methods on this
/// struct — neither `gix` nor `Command::new("git")` is exposed.
pub struct GitClient {
    repo: gix::ThreadSafeRepository,
    workdir: PathBuf,
}

impl GitClient {
    /// Open an existing repository at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GitError> {
        let path = path.as_ref();
        let repo = gix::ThreadSafeRepository::open(path).map_err(|source| GitError::OpenRepo {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        let workdir = repo
            .work_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        Ok(Self { repo, workdir })
    }

    /// Working tree status against HEAD.
    pub async fn status(&self) -> Result<Vec<StatusEntry>, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<Vec<StatusEntry>, GitError> {
            let repo = repo.to_thread_local();
            let platform = repo
                .status(gix::progress::Discard)
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let iter = platform
                .into_iter(None)
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let mut out = Vec::new();
            for item in iter {
                let item = item.map_err(|e| GitError::Gix(e.to_string()))?;
                out.push(StatusEntry::from_item(&item));
            }
            Ok(out)
        })
        .await?
    }

    /// Unified diff of `HEAD` against its first parent (`HEAD~`).
    ///
    /// Returns an empty string when `HEAD` has no parent (initial commit).
    pub async fn diff_head_parent(&self) -> Result<String, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<String, GitError> {
            let repo = repo.to_thread_local();
            let head = repo
                .head_commit()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let parents: Vec<_> = head.parent_ids().collect();
            let Some(parent_id) = parents.first() else {
                return Ok(String::new());
            };
            let parent = repo
                .find_object(*parent_id)
                .map_err(|e| GitError::Gix(e.to_string()))?
                .try_into_commit()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let head_tree = head.tree().map_err(|e| GitError::Gix(e.to_string()))?;
            let parent_tree = parent.tree().map_err(|e| GitError::Gix(e.to_string()))?;
            let mut changes = parent_tree
                .changes()
                .map_err(|e| GitError::Gix(e.to_string()))?;
            let mut buf = String::new();
            changes
                .for_each_to_obtain_tree(
                    &head_tree,
                    |change| -> Result<_, std::convert::Infallible> {
                        use std::fmt::Write as _;
                        let _ = writeln!(buf, "{}", change.location());
                        Ok(std::ops::ControlFlow::Continue(()))
                    },
                )
                .map_err(|e| GitError::Gix(e.to_string()))?;
            Ok(buf)
        })
        .await?
    }

    /// Linked worktrees registered with the repository.
    pub async fn worktrees(&self) -> Result<Vec<WorktreeInfo>, GitError> {
        let repo = self.repo.clone();
        spawn_blocking(move || -> Result<Vec<WorktreeInfo>, GitError> {
            let repo = repo.to_thread_local();
            let proxies = repo.worktrees().map_err(|e| GitError::Gix(e.to_string()))?;
            let mut out = Vec::with_capacity(proxies.len());
            for proxy in proxies {
                let path = proxy.base().map_err(|e| GitError::Gix(e.to_string()))?;
                let branch = proxy
                    .into_repo_with_possibly_inaccessible_worktree()
                    .ok()
                    .and_then(|wt| wt.head_name().ok().flatten())
                    .map(|name| name.shorten().to_string());
                out.push(WorktreeInfo { path, branch });
            }
            Ok(out)
        })
        .await?
    }

    /// Create a new linked worktree at `.wrapix/worktree/<label>/<bead_id>/`
    /// on a fresh branch `loom/<label>/<bead_id>` based on `HEAD`.
    pub async fn create_worktree(
        &self,
        label: &SpecLabel,
        bead_id: &BeadId,
    ) -> Result<CreatedWorktree, GitError> {
        let branch = format!("{BRANCH_PREFIX}/{label}/{bead_id}");
        let rel = PathBuf::from(WORKTREE_BASE)
            .join(label.as_str())
            .join(bead_id.as_str());
        let path = self.workdir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let path_arg: OsString = path.clone().into();
        run_git(
            &self.workdir,
            ["worktree", "add", "-b", &branch],
            Some(&path_arg),
        )
        .await?;

        Ok(CreatedWorktree { path, branch })
    }

    /// Remove a linked worktree (force-removed; pruned afterwards).
    pub async fn remove_worktree(&self, path: &Path) -> Result<(), GitError> {
        let path_str = path.to_string_lossy().into_owned();
        run_git(
            &self.workdir,
            ["worktree", "remove", "--force", &path_str],
            None,
        )
        .await?;
        run_git(&self.workdir, ["worktree", "prune"], None).await?;
        Ok(())
    }

    /// Force-delete the named branch. Used by the parallel batch driver to
    /// reclaim the per-bead branch after agent failure (the worktree has
    /// already been removed by [`Self::remove_worktree`]). A non-existent
    /// branch surfaces as [`GitError::GitCli`] — call only when the branch
    /// is known to exist.
    pub async fn delete_branch(&self, branch: &str) -> Result<(), GitError> {
        run_git(&self.workdir, ["branch", "-D", branch], None).await?;
        Ok(())
    }

    /// Merge `branch` into the current driver branch. Returns
    /// [`MergeResult::Conflict`] when git reports merge conflicts; other
    /// failures surface as [`GitError`].
    pub async fn merge_branch(&self, branch: &str) -> Result<MergeResult, GitError> {
        let output = run_git_raw(
            &self.workdir,
            ["merge", "--no-ff", "--no-edit", branch],
            None,
        )
        .await?;

        if output.status.success() {
            return Ok(MergeResult::Ok);
        }

        // Conflict: git exits non-zero and leaves the index dirty. Detect via
        // `git ls-files --unmerged` rather than scraping merge stderr.
        let unmerged = run_git_raw(&self.workdir, ["ls-files", "--unmerged"], None).await?;
        if unmerged.status.success() && !unmerged.stdout.is_empty() {
            return Ok(MergeResult::Conflict);
        }

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(GitError::GitCli {
            status: output.status.code().unwrap_or(-1),
            stderr,
        })
    }
}

/// Result of [`GitClient::create_worktree`].
#[derive(Debug, Clone)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Linked worktree as reported by `gix`.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// Working tree status entry.
#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
}

impl StatusEntry {
    fn from_item(item: &gix::status::Item) -> Self {
        let path = item.location().to_string();
        let kind = match item {
            gix::status::Item::IndexWorktree(_) => StatusKind::WorktreeChange,
            gix::status::Item::TreeIndex(_) => StatusKind::IndexChange,
        };
        Self { path, kind }
    }
}

/// Kind of change reported by [`StatusEntry`]. Coarse on purpose — callers
/// that need richer detail (rename detection, etc.) should grow this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    IndexChange,
    WorktreeChange,
}

/// Outcome of [`GitClient::merge_branch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeResult {
    Ok,
    Conflict,
}

/// Run `git` with an explicit `-C <workdir>`, no shell, 60s ceiling. Returns
/// `Ok(())` only on a clean exit.
async fn run_git<I, S>(workdir: &Path, args: I, trailing: Option<&OsString>) -> Result<(), GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = run_git_raw(workdir, args, trailing).await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr,
    })
}

async fn run_git_raw<I, S>(
    workdir: &Path,
    args: I,
    trailing: Option<&OsString>,
) -> Result<std::process::Output, GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(workdir);
    let mut argv_for_log: Vec<String> = Vec::new();
    for arg in args {
        argv_for_log.push(arg.as_ref().to_string_lossy().into_owned());
        cmd.arg(arg);
    }
    if let Some(t) = trailing {
        argv_for_log.push(t.to_string_lossy().into_owned());
        cmd.arg(t);
    }

    let fut = cmd.output();
    match timeout(GIT_TIMEOUT, fut).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(GitError::Spawn(e)),
        Err(_) => Err(GitError::GitTimeout {
            args: argv_for_log.join(" "),
        }),
    }
}
