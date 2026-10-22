use super::*;
use wrix_core::git::Branch;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn interrupted_linked_worktree_cutover_repairs_to_private_git_without_losing_staged_state()
-> TestResult {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository)?;
    git(&repository, &["init", "-q"])?;
    fs::write(repository.join("tracked"), "committed")?;
    git(&repository, &["add", "."])?;
    git(
        &repository,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=f@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "initial",
        ],
    )?;
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "ssh://git@example.invalid/fixed-repository",
        ],
    )?;
    let source = repository.join(".git/beads-worktrees/beads");
    git(
        &repository,
        &["worktree", "add", "-qb", "beads", source.to_str().unwrap()],
    )?;
    fs::write(source.join("tracked"), "staged")?;
    git(&source, &["add", "tracked"])?;
    git(&source, &["update-index", "--split-index"])?;
    fs::write(source.join("tracked"), "unstaged")?;
    fs::write(source.join("untracked"), "retained untracked")?;
    let head = git_output(&source, &["rev-parse", "HEAD"])?;
    fs::write(
        repository.join(".git/unrelated-secret"),
        "unrelated canonical object",
    )?;
    let unrelated = git_output(&repository, &["hash-object", "-w", ".git/unrelated-secret"])?;
    let namespace = temporary.path().join("namespace");
    let destination = namespace.join("beads-sync/beads");
    fs::create_dir_all(destination.parent().unwrap())?;
    // This is exactly the interrupted rename state, with .git still pointing
    // into the authoritative commonDir. The retry must convert it, not return.
    fs::rename(&source, &destination)?;
    let private = temporary.path().join("private");
    migrate_worktree(&repository, &Branch::parse("beads")?, &namespace, &private)?;
    let host_git = private.join("repository");
    let host_output = |args: &[&str]| -> Result<String> {
        let mut scoped = vec![
            "--git-dir",
            host_git.to_str().unwrap(),
            "--work-tree",
            destination.to_str().unwrap(),
        ];
        scoped.extend_from_slice(args);
        git_output(&namespace, &scoped)
    };
    assert_eq!(host_output(&["rev-parse", "HEAD"])?, head);
    assert_eq!(host_output(&["show", ":tracked"])?, "staged");
    assert_eq!(fs::read_to_string(destination.join("tracked"))?, "unstaged");
    assert_eq!(
        fs::read_to_string(destination.join("untracked"))?,
        "retained untracked"
    );
    assert_eq!(host_output(&["rev-parse", "--shared-index-path"])?, "");
    assert_eq!(
        host_output(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?,
        host_git.to_str().unwrap()
    );
    assert_eq!(
        host_output(&["remote", "get-url", "origin"])?,
        "ssh://git@example.invalid/fixed-repository"
    );
    assert_eq!(
        host_output(&["config", "branch.beads.merge"])?,
        "refs/heads/beads"
    );
    migrate_worktree(&repository, &Branch::parse("beads")?, &namespace, &private)?;
    host_output(&["update-ref", "refs/heads/worker-only", &head])?;
    assert!(namespace.join("guest-git").is_dir());
    assert!(!destination.join(".git").exists());
    for metadata in [&host_git, &namespace.join("guest-git")] {
        assert!(
            !Command::new("git")
                .arg("--git-dir")
                .arg(metadata)
                .args(["cat-file", "-e", &unrelated])
                .output()?
                .status
                .success(),
            "standalone clone leaked unrelated canonical Git objects"
        );
    }
    let refs = git_output(&repository, &["for-each-ref", "--format=%(refname)"])?;
    assert!(
        !refs.contains("worker-only"),
        "worker metadata must not write canonical refs"
    );
    assert_eq!(fs::read_to_string(repository.join("tracked"))?, "committed");
    Ok(())
}

#[test]
fn shared_record_rejects_symlink_and_non_private_files() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("shared.json");
    let other = temporary.path().join("other.json");
    fs::write(
        &other,
        br#"{"version":1,"tcp":false,"sql_port":null,"sync_port":null}"#,
    )?;
    fs::set_permissions(&other, fs::Permissions::from_mode(0o600))?;
    symlink(&other, &path)?;
    assert!(Record::at(temporary.path()).is_err());
    fs::remove_file(&path)?;
    fs::rename(&other, &path)?;
    assert!(Record::at(temporary.path())?.is_some());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(Record::at(temporary.path()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    for value in [
        r#"{"version":1,"tcp":false,"sql_port":3306,"sync_port":null}"#,
        r#"{"version":1,"tcp":false,"sql_port":null,"sync_port":3307}"#,
        r#"{"version":1,"tcp":true,"sql_port":3306,"sync_port":3306}"#,
    ] {
        fs::write(&path, value)?;
        assert!(Record::at(temporary.path()).is_err());
    }
    Ok(())
}
