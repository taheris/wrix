//! Integration tests for `loom_core::git::GitClient`.
//!
//! Each test builds a throwaway repo in a `tempdir` via the system `git`
//! binary, opens it through the typed client, and asserts the documented
//! behaviour for create/remove worktree and merge-back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use loom_core::git::{GitClient, MergeResult};
use loom_core::identifier::{BeadId, SpecLabel};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .with_context(|| format!("spawn git {args:?}"))?;
    anyhow::ensure!(status.success(), "git {args:?} exited with {status}");
    Ok(())
}

fn init_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"])?;
    git(path, &["config", "user.email", "test@example.com"])?;
    git(path, &["config", "user.name", "Test"])?;
    git(path, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-q", "-m", "initial"])?;
    Ok(dir)
}

#[tokio::test]
async fn create_and_remove_worktree_round_trip() -> Result<()> {
    let repo = init_repo()?;
    let client = GitClient::open(repo.path())?;

    let label = SpecLabel::new("loom-harness");
    let bead = BeadId::new("wx-3hhwq.6")?;
    let created = client.create_worktree(&label, &bead).await?;

    assert!(
        created.path.exists(),
        "worktree path {:?} should exist on disk",
        created.path
    );
    assert_eq!(created.branch, "loom/loom-harness/wx-3hhwq.6");
    assert!(
        created.path.ends_with("loom-harness/wx-3hhwq.6"),
        "worktree path should end with <label>/<bead-id>: {:?}",
        created.path
    );

    let listed = client.worktrees().await?;
    assert!(
        listed.iter().any(|w| w.path == created.path),
        "gix worktrees() should list the new worktree: got {listed:?}",
    );

    client.remove_worktree(&created.path).await?;
    assert!(
        !created.path.exists(),
        "worktree path {:?} should be cleaned up",
        created.path
    );

    let after = client.worktrees().await?;
    assert!(
        !after.iter().any(|w| w.path == created.path),
        "removed worktree should not appear in worktrees(): got {after:?}",
    );

    Ok(())
}

#[tokio::test]
async fn merge_branch_clean_returns_ok() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();

    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("feature.txt"), "added on feature\n")?;
    git(path, &["add", "feature.txt"])?;
    git(path, &["commit", "-q", "-m", "feature commit"])?;
    git(path, &["checkout", "-q", "main"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Ok);
    assert!(path.join("feature.txt").exists());
    Ok(())
}

#[tokio::test]
async fn merge_branch_conflict_is_reported() -> Result<()> {
    let repo = init_repo()?;
    let path = repo.path();

    // Branch A: rewrite README on `feature`.
    git(path, &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(path.join("README.md"), "feature line\n")?;
    git(path, &["commit", "-q", "-am", "feature edit"])?;

    // Branch B: rewrite same line on `main`.
    git(path, &["checkout", "-q", "main"])?;
    std::fs::write(path.join("README.md"), "main line\n")?;
    git(path, &["commit", "-q", "-am", "main edit"])?;

    let client = GitClient::open(path)?;
    let result = client.merge_branch("feature").await?;

    assert_eq!(result, MergeResult::Conflict);
    Ok(())
}
