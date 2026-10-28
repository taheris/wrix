use super::*;
use wrix_core::git::Branch;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn fresh_native_tcp_state_uses_the_already_bound_controller_sql_port() -> TestResult {
    let root = tempfile::tempdir()?;
    let guard = coordination::acquire(&root.path().join("host-lifecycle"))?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = NonZeroU16::new(listener.local_addr()?.port()).ok_or("zero port")?;
    let record = Record::prepare_native_for(
        &guard,
        port,
        NonZeroU16::new(23005).ok_or("zero sync port")?,
        DoltTransport::Tcp,
    )?;
    assert_eq!(record.public_sql_port(), Some(port));
    assert_ne!(record.sync_port, Some(port));
    assert_eq!(
        Record::at(guard.root())?
            .ok_or("missing shared state")?
            .public_sql_port(),
        Some(port)
    );
    Ok(())
}

#[test]
fn retained_native_tcp_endpoint_conflict_refuses_without_rewriting_state() -> TestResult {
    let root = tempfile::tempdir()?;
    let guard = coordination::acquire(&root.path().join("host-lifecycle"))?;
    let first = NonZeroU16::new(23001).ok_or("zero port")?;
    let second = NonZeroU16::new(23002).ok_or("zero port")?;
    Record::prepare_native_for(
        &guard,
        first,
        NonZeroU16::new(23005).ok_or("zero sync port")?,
        DoltTransport::Tcp,
    )?;
    let before = fs::read(guard.root().join("shared.json"))?;
    for (sql, sync) in [(second, 23005), (first, 23006)] {
        assert!(
            Record::prepare_native_for(
                &guard,
                sql,
                NonZeroU16::new(sync).ok_or("zero sync port")?,
                DoltTransport::Tcp
            )
            .is_err()
        );
        assert_eq!(fs::read(guard.root().join("shared.json"))?, before);
    }
    Ok(())
}

#[test]
fn interrupted_linked_worktree_cutover_repairs_to_private_git_without_losing_staged_state()
-> TestResult {
    exercise_linked_worktree_cutover(WorktreeView::HostAbsolute)
}

#[test]
fn interrupted_relative_linked_worktree_cutover_preserves_staged_and_dirty_state() -> TestResult {
    exercise_linked_worktree_cutover(WorktreeView::HostRelative)
}

#[test]
fn interrupted_container_path_worktree_cutover_preserves_private_import_and_dirty_state()
-> TestResult {
    exercise_linked_worktree_cutover(WorktreeView::ContainerAbsolute)
}

#[derive(Clone, Copy)]
enum WorktreeView {
    HostAbsolute,
    HostRelative,
    ContainerAbsolute,
}

fn exercise_linked_worktree_cutover(view: WorktreeView) -> TestResult {
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
    let mut arguments = vec!["worktree", "add"];
    if matches!(view, WorktreeView::HostRelative) {
        arguments.push("--relative-paths");
    }
    arguments.extend(["-qb", "beads", source.to_str().unwrap()]);
    git(&repository, &arguments)?;
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
    if matches!(view, WorktreeView::ContainerAbsolute) {
        // Actual retained host state: a worker's Git repaired both directions
        // through its /workspace mount. Never open this backend-visible pointer.
        fs::write(
            repository.join(".git/worktrees/beads/gitdir"),
            "/workspace/.git/beads-worktrees/beads/.git\n",
        )?;
        fs::write(
            source.join(".git"),
            "gitdir: /workspace/.git/worktrees/beads\n",
        )?;
        let unrelated = repository.join(".git/worktrees/active-host-bead");
        fs::create_dir_all(&unrelated)?;
        fs::write(unrelated.join("gitdir"), "/host/active-bead/.git\n")?;
        fs::write(
            unrelated.join("locked"),
            "Active host worktree; preserve registration across container path views\n",
        )?;
    }
    // This is exactly the interrupted rename state, with .git still pointing
    // into the authoritative commonDir. The retry must convert it, not return.
    fs::rename(&source, &destination)?;
    let private = temporary.path().join("private");
    if !matches!(view, WorktreeView::HostAbsolute) {
        // Retain the same partial private clone as a failed host attempt, not
        // just the initial moved worktree. Retry must finish without resetting it.
        crate::dolt::private_directory(&private)?;
        git(
            &repository,
            &[
                "clone",
                "--no-local",
                "--single-branch",
                "--no-checkout",
                "--branch",
                "beads",
                repository.to_str().unwrap(),
                private.join("git-import").to_str().unwrap(),
            ],
        )?;
    }
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
    if matches!(view, WorktreeView::ContainerAbsolute) {
        assert_eq!(
            fs::read_to_string(repository.join(".git/worktrees/active-host-bead/gitdir"))?,
            "/host/active-bead/.git\n"
        );
        assert_eq!(
            fs::read_to_string(repository.join(".git/worktrees/active-host-bead/locked"))?,
            "Active host worktree; preserve registration across container path views\n"
        );
    }
    Ok(())
}

#[test]
fn moved_backend_git_pointer_cannot_select_unrelated_host_administration() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    let admin = repository.join(".git/worktrees/other");
    fs::create_dir_all(&admin)?;
    fs::write(admin.join("gitdir"), "../../../unrelated/.git\n")?;
    let source = repository.join(".git/beads-worktrees/beads");
    let destination = temporary.path().join("namespace/beads-sync/beads");
    fs::create_dir_all(&destination)?;
    fs::write(
        destination.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )?;
    assert!(linked_admin(&repository, &source, &destination, "beads")?.is_none());
    assert!(lexical_absolute(Path::new("/../../escape")).is_none());
    Ok(())
}

#[test]
fn container_path_admin_selection_rejects_wrong_branch_and_duplicate_claims() -> TestResult {
    for duplicate in [false, true] {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("repository");
        let base = repository.join(".git/worktrees");
        let admin = base.join("beads");
        fs::create_dir_all(&admin)?;
        fs::write(
            admin.join("gitdir"),
            "/workspace/.git/beads-worktrees/beads/.git\n",
        )?;
        fs::write(
            admin.join("HEAD"),
            if duplicate {
                "ref: refs/heads/beads\n"
            } else {
                "ref: refs/heads/other\n"
            },
        )?;
        if duplicate {
            let other = base.join("duplicate");
            fs::create_dir_all(&other)?;
            fs::write(
                other.join("gitdir"),
                "/workspace/.git/beads-worktrees/beads/.git\n",
            )?;
            fs::write(other.join("HEAD"), "ref: refs/heads/beads\n")?;
        }
        let source = repository.join(".git/beads-worktrees/beads");
        let destination = temporary.path().join("namespace/beads-sync/beads");
        assert!(linked_admin(&repository, &source, &destination, "beads").is_err());
        assert_eq!(
            fs::read_to_string(admin.join("gitdir"))?,
            "/workspace/.git/beads-worktrees/beads/.git\n"
        );
    }
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
