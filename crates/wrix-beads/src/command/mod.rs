use std::{
    env, fmt, fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Output, Stdio},
};

use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_core::{
    beads_config::{ReadError, read_sync_branch},
    git::Branch,
};

pub type Result<T> = std::result::Result<T, Error>;

const MAX_GITHUB_BLOB_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Display, ThisError)]
pub enum IssueIdParseError {
    /// invalid beads issue identifier: {value}
    Invalid { value: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IssueId(String);

impl IssueId {
    fn parse(input: &str) -> std::result::Result<Self, IssueIdParseError> {
        let Some((prefix, local)) = input.split_once('-') else {
            return Err(IssueIdParseError::Invalid {
                value: input.to_owned(),
            });
        };
        let valid_segment = |segment: &str| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        };
        if !valid_segment(prefix) || !local.split('.').all(valid_segment) {
            return Err(IssueIdParseError::Invalid {
                value: input.to_owned(),
            });
        }
        Ok(Self(input.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// beads workflow I/O failed: {source}
    Io {
        #[from]
        source: io::Error,
    },
    /// invalid beads sync branch: {source}
    BeadsConfig {
        #[from]
        source: ReadError,
    },
    /// invalid issue identifier returned by beads: {source}
    InvalidIssueId {
        #[from]
        source: IssueIdParseError,
    },
    /// {program} failed: {stderr}
    CommandFailed {
        program: &'static str,
        stderr: String,
    },
    /// Dolt remote file {path} is {bytes} bytes, exceeding GitHub's 100 MiB limit; no sync commit was created. Back up and losslessly repack the file remote before retrying; do not delete database history
    OversizedRemoteFile { path: String, bytes: u64 },
    /// unpublished beads history contains a Git blob exceeding GitHub's 100 MiB limit. Back up the local branch, losslessly repack the file remote, and replace only unpublished sync commits; do not force-push published history
    OversizedSyncHistory,
    /// unsafe beads worktree path: {path}; recovery requires directories beneath the repository's .git without symlink components
    UnsafeWorktreePath { path: String },
    /// staged Dolt remote already exists at {path}
    StagedRemoteExists { path: String },
    /// worktree recovery failed: {recovery}; restoring the staged Dolt remote also failed: {restore}
    RecoveryRestore {
        recovery: Box<Self>,
        restore: Box<Self>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Push,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "push" => Some(Self::Push),
            _ => None,
        }
    }
}

pub const HELP: &str = "Manage beads workflows.\n\nUsage: wrix beads <command>\n\nCommands:\n  push  Synchronize session-close beads state.\n\nOptions:\n  -h, --help  Print help.\n";

pub fn write_help(stdout: &mut impl Write) -> Result<()> {
    stdout.write_all(HELP.as_bytes())?;
    Ok(())
}

pub fn run(command: Command, stdout: &mut impl Write, stderr: &mut impl Write) -> Result<ExitCode> {
    match command {
        Command::Push => push(stdout, stderr),
    }
}

fn push(stdout: &mut impl Write, stderr: &mut impl Write) -> Result<ExitCode> {
    if env::var_os("LOOM_INSIDE").is_some() {
        writeln!(
            stderr,
            "wrix beads push: LOOM_INSIDE set; loom driver owns publish, skipping"
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    let current_dir = env::current_dir()?;
    let Some(context) = Context::load(&current_dir)? else {
        writeln!(
            stderr,
            "wrix beads push: cannot resolve a git repository from '{}' — run inside a workspace checkout",
            current_dir.display()
        )?;
        return Ok(ExitCode::FAILURE);
    };

    env::set_current_dir(&context.root)?;
    disable_auto_export()?;
    restore_staged_dolt_remote(&context)?;
    let remote_override = prepare_dolt_origin_remote(&context, stderr)?;
    let sync_result = sync_dolt_remote(stderr);
    let restore_result = remote_override.restore();
    let sync_status = sync_result?;
    restore_result?;
    if sync_status != ExitCode::SUCCESS {
        return Ok(ExitCode::FAILURE);
    }
    sync_beads_git_branch(&context, stdout, stderr)
}

struct Context {
    root: PathBuf,
    branch: Branch,
    worktree: PathBuf,
    worktree_remote_dir: PathBuf,
    recovery_remote_dir: PathBuf,
}

impl Context {
    fn load(current_dir: &Path) -> Result<Option<Self>> {
        let output = ProcessCommand::new("git")
            .arg("rev-parse")
            .arg("--show-toplevel")
            .current_dir(current_dir)
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let mut root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if let Some(peel) = peel_beads_worktree(&root) {
            root = peel;
        }
        let root = root.canonicalize()?;
        let branch = read_sync_branch(&root)?;
        let worktree = root.join(".git/beads-worktrees").join(branch.as_str());
        let worktree_remote_dir = worktree.join(".beads/dolt-remote");
        let recovery_remote_dir = root.join(".git/wrix-beads-dolt-remote-recovery");
        let context = Self {
            root,
            branch,
            worktree,
            worktree_remote_dir,
            recovery_remote_dir,
        };
        context.check_worktree_paths()?;
        Ok(Some(context))
    }

    fn check_worktree_paths(&self) -> Result<()> {
        let git_dir = self.root.join(".git");
        check_managed_directory(&git_dir, &self.worktree_remote_dir)?;
        check_managed_directory(&git_dir, &self.recovery_remote_dir)
    }
}

fn check_managed_directory(base: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_source| Error::UnsafeWorktreePath {
            path: path.display().to_string(),
        })?;
    if relative.as_os_str().is_empty() {
        return Err(Error::UnsafeWorktreePath {
            path: path.display().to_string(),
        });
    }
    let mut current = base.to_path_buf();
    check_directory_component(&current)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(Error::UnsafeWorktreePath {
                path: path.display().to_string(),
            });
        };
        current.push(name);
        check_directory_component(&current)?;
    }
    Ok(())
}

fn check_directory_component(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(Error::UnsafeWorktreePath {
            path: path.display().to_string(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn peel_beads_worktree(root: &Path) -> Option<PathBuf> {
    let text = root.to_string_lossy();
    text.find("/.git/beads-worktrees/")
        .map(|index| PathBuf::from(&text[..index]))
}

fn prepare_dolt_origin_remote(
    context: &Context,
    stderr: &mut impl Write,
) -> Result<DoltRemoteOverride> {
    if !context.worktree_remote_dir.is_dir() {
        return Ok(DoltRemoteOverride::inactive());
    }

    let remote = format!("file://{}", context.worktree_remote_dir.display());
    let list = run_output("bd", &["dolt", "remote", "list"])?;
    if !list.status.success() {
        return Err(Error::CommandFailed {
            program: "bd dolt remote list (database unavailable; remote state unknown)",
            stderr: String::from_utf8_lossy(&list.stderr).into_owned(),
        });
    }
    let text = String::from_utf8_lossy(&list.stdout);
    let origin = origin_remote_url(&text).map(ToOwned::to_owned);
    if origin.as_deref() == Some(remote.as_str()) {
        return Ok(DoltRemoteOverride::inactive());
    }

    if env::var_os("IS_SANDBOX").is_some() || context.root.starts_with("/workspace") {
        return DoltRemoteOverride::install_temporary(origin, &remote, stderr);
    }

    writeln!(
        stderr,
        "wrix beads push: repairing Dolt origin remote -> {remote}"
    )?;
    replace_dolt_origin(origin.as_deref(), &remote)?;
    Ok(DoltRemoteOverride::inactive())
}

struct DoltRemoteOverride {
    original: Option<String>,
    active: bool,
}

impl DoltRemoteOverride {
    const fn inactive() -> Self {
        Self {
            original: None,
            active: false,
        }
    }

    fn install_temporary(
        original: Option<String>,
        remote: &str,
        stderr: &mut impl Write,
    ) -> Result<Self> {
        writeln!(
            stderr,
            "wrix beads push: temporarily using sandbox Dolt origin remote -> {remote}"
        )?;
        replace_dolt_origin(original.as_deref(), remote)?;
        Ok(Self {
            original,
            active: true,
        })
    }

    fn restore(self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        run_required("bd", &["sql", "CALL DOLT_REMOTE('remove', 'origin')"])?;
        if let Some(remote) = self.original {
            let add = format!("CALL DOLT_REMOTE('add', 'origin', {})", sql_quote(&remote));
            run_required("bd", &["sql", &add])?;
        }
        Ok(())
    }
}

fn replace_dolt_origin(existing: Option<&str>, remote: &str) -> Result<()> {
    if existing.is_some() {
        run_required("bd", &["sql", "CALL DOLT_REMOTE('remove', 'origin')"])?;
    }
    let add = format!("CALL DOLT_REMOTE('add', 'origin', {})", sql_quote(remote));
    run_required("bd", &["sql", &add])
}

fn origin_remote_url(remote_list: &str) -> Option<&str> {
    remote_list.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("origin") {
            fields.next()
        } else {
            None
        }
    })
}

fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn disable_auto_export() -> Result<()> {
    run_required("bd", &["config", "set", "export.auto", "false"])
}

fn sync_dolt_remote(stderr: &mut impl Write) -> Result<ExitCode> {
    let commit = run_output("bd", &["dolt", "commit"])?;
    if !commit.status.success() && !commit.stderr.is_empty() {
        stderr.write_all(&commit.stderr)?;
    }

    let push = run_output("bd", &["dolt", "push"])?;
    if !push.stderr.is_empty() {
        stderr.write_all(&push.stderr)?;
    }
    if push.status.success() {
        return Ok(ExitCode::SUCCESS);
    }
    if !is_fast_forward_rejection(&push.stderr) && !is_fast_forward_rejection(&push.stdout) {
        return Ok(ExitCode::FAILURE);
    }
    pull_with_intent_protection(stderr)
}

fn is_fast_forward_rejection(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    [
        "non-fast-forward",
        "non fast forward",
        "not a fast-forward",
        "cannot fast forward",
        "remote contains work",
        "fetch first",
        "behind",
        "out of date",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn pull_with_intent_protection(stderr: &mut impl Write) -> Result<ExitCode> {
    let affected_ids = query_affected_ids()?;
    if affected_ids.is_empty() {
        run_required("bd", &["dolt", "pull"])?;
        let push = run_output("bd", &["dolt", "push"])?;
        return Ok(status_to_exit(&push));
    }

    let snapshot_query = snapshot_query_for_ids(&affected_ids);
    let intent = run_required_output("bd", &["sql", "--csv", &snapshot_query])?;
    run_required("bd", &["dolt", "pull"])?;
    let post = run_required_output("bd", &["sql", "--csv", &snapshot_query])?;
    if intent.stdout != post.stdout {
        writeln!(
            stderr,
            "wrix beads push: pull-fallback diverged from local status/label intent; refusing to push"
        )?;
        writeln!(
            stderr,
            "wrix beads push: affected issue IDs: {}",
            affected_ids
                .iter()
                .map(IssueId::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        )?;
        return Ok(ExitCode::FAILURE);
    }
    let push = run_output("bd", &["dolt", "push"])?;
    Ok(status_to_exit(&push))
}

fn query_affected_ids() -> Result<Vec<IssueId>> {
    let output = run_required_output("bd", &["sql", "--csv", AFFECTED_IDS_SQL])?;
    parse_affected_ids(&output.stdout).map_err(Error::from)
}

fn parse_affected_ids(output: &[u8]) -> std::result::Result<Vec<IssueId>, IssueIdParseError> {
    let text = String::from_utf8_lossy(output);
    text.lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(IssueId::parse)
        .collect()
}

const AFFECTED_IDS_SQL: &str = "\n    SELECT DISTINCT id FROM (\n      SELECT to_id AS id\n      FROM dolt_commit_diff_issues\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n        AND (from_status IS NULL OR from_status <> to_status)\n      UNION\n      SELECT to_issue_id AS id\n      FROM dolt_commit_diff_labels\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n      UNION\n      SELECT from_issue_id AS id\n      FROM dolt_commit_diff_labels\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n    ) AS touched\n    WHERE id IS NOT NULL\n";

fn snapshot_query_for_ids(ids: &[IssueId]) -> String {
    let in_list = ids
        .iter()
        .map(|id| sql_quote(id.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT i.id, i.status, COALESCE((SELECT GROUP_CONCAT(label ORDER BY label SEPARATOR ',') FROM labels WHERE issue_id = i.id), '') AS labels FROM issues i WHERE i.id IN ({in_list}) ORDER BY i.id"
    )
}

fn sync_beads_git_branch(
    context: &Context,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<ExitCode> {
    if !ensure_beads_worktree(context, stderr)? {
        return Ok(ExitCode::SUCCESS);
    }
    context.check_worktree_paths()?;
    repair_worktree_pointers(context)?;
    commit_dirty_worktree(&context.worktree)?;
    run_git_required_in(&context.worktree, &["pull", "--rebase", "--quiet"])?;

    commit_dirty_worktree(&context.worktree)?;
    run_git_required_in(
        &context.worktree,
        &["push", "-u", "origin", context.branch.as_str(), "--quiet"],
    )?;
    writeln!(stdout, "wrix beads push: synced to GitHub")?;
    Ok(ExitCode::SUCCESS)
}

fn ensure_beads_worktree(context: &Context, stderr: &mut impl Write) -> Result<bool> {
    context.check_worktree_paths()?;
    if context.worktree.is_dir()
        && run_git_output_in(&context.worktree, &["rev-parse", "--is-inside-work-tree"])?
            .status
            .success()
    {
        return Ok(true);
    }
    if context.worktree.is_dir() {
        run_git_required(&["worktree", "prune"])?;
    }
    let staged_remote = stage_worktree_dolt_remote(context)?;
    let recreate_result = recreate_beads_worktree(context, stderr);
    if !staged_remote {
        return recreate_result;
    }
    let restore_result = restore_staged_dolt_remote(context);
    match (recreate_result, restore_result) {
        (Ok(recreated), Ok(())) => Ok(recreated),
        (Err(recreate_error), Ok(())) => Err(recreate_error),
        (Ok(_), Err(restore_error)) => Err(restore_error),
        (Err(recovery), Err(restore)) => Err(Error::RecoveryRestore {
            recovery: Box::new(recovery),
            restore: Box::new(restore),
        }),
    }
}

fn stage_worktree_dolt_remote(context: &Context) -> Result<bool> {
    context.check_worktree_paths()?;
    if !context.worktree_remote_dir.is_dir() {
        return Ok(false);
    }
    if context.recovery_remote_dir.exists() {
        return Err(Error::StagedRemoteExists {
            path: context.recovery_remote_dir.display().to_string(),
        });
    }
    fs::rename(&context.worktree_remote_dir, &context.recovery_remote_dir)?;
    Ok(true)
}

fn restore_staged_dolt_remote(context: &Context) -> Result<()> {
    context.check_worktree_paths()?;
    if !context.recovery_remote_dir.is_dir() {
        return Ok(());
    }
    if context.worktree_remote_dir.exists() {
        fs::remove_dir_all(&context.worktree_remote_dir)?;
    }
    fs::create_dir_all(context.worktree.join(".beads"))?;
    fs::rename(&context.recovery_remote_dir, &context.worktree_remote_dir)?;
    Ok(())
}

fn recreate_beads_worktree(context: &Context, stderr: &mut impl Write) -> Result<bool> {
    context.check_worktree_paths()?;
    if context.worktree.is_dir() {
        fs::remove_dir_all(&context.worktree)?;
    }
    if run_git_output(&["rev-parse", "--verify", context.branch.as_str()])?
        .status
        .success()
    {
        let worktree = context.worktree_text();
        run_git_required(&[
            "worktree",
            "add",
            &worktree,
            context.branch.as_str(),
            "--quiet",
        ])?;
    } else {
        let origin_branch = format!("origin/{}", context.branch);
        if run_git_output(&["rev-parse", "--verify", &origin_branch])?
            .status
            .success()
        {
            let worktree = context.worktree_text();
            run_git_required(&[
                "worktree",
                "add",
                "-b",
                context.branch.as_str(),
                &worktree,
                &origin_branch,
                "--quiet",
            ])?;
        } else {
            writeln!(
                stderr,
                "wrix beads push: no '{}' branch found; skipping GitHub sync",
                context.branch
            )?;
            return Ok(false);
        }
    }
    Ok(true)
}

impl Context {
    fn worktree_text(&self) -> String {
        self.worktree.display().to_string()
    }
}

fn repair_worktree_pointers(context: &Context) -> Result<()> {
    let dotgit = context.worktree.join(".git");
    let admin = context
        .root
        .join(".git/worktrees")
        .join(context.branch.as_str());
    if !dotgit.is_file() || !admin.is_dir() {
        return Ok(());
    }
    let dotgit_content = fs::read_to_string(&dotgit)?;
    let Some(current) = dotgit_content.strip_prefix("gitdir: ") else {
        return Ok(());
    };
    let current = current.trim();
    if !Path::new(current).is_dir() {
        fs::write(&dotgit, format!("gitdir: {}\n", admin.display()))?;
        fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", context.worktree.display()),
        )?;
    }
    Ok(())
}

fn reject_oversized_remote_files(directory: &Path) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            reject_oversized_remote_files(&entry.path())?;
        } else if kind.is_file() {
            let bytes = entry.metadata()?.len();
            if bytes > MAX_GITHUB_BLOB_BYTES {
                return Err(Error::OversizedRemoteFile {
                    path: entry.path().display().to_string(),
                    bytes,
                });
            }
        }
    }
    Ok(())
}

fn reject_oversized_sync_history(worktree: &Path) -> Result<()> {
    let filter = format!("--filter=blob:limit={}", MAX_GITHUB_BLOB_BYTES + 1);
    let output = run_git_required_output_in(
        worktree,
        &[
            "rev-list",
            "--objects",
            &filter,
            "--filter-print-omitted",
            "--no-object-names",
            "HEAD",
            "--not",
            "--remotes=origin",
        ],
    )?;
    if output
        .stdout
        .split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"~"))
    {
        return Err(Error::OversizedSyncHistory);
    }
    Ok(())
}

fn commit_dirty_worktree(worktree: &Path) -> Result<()> {
    reject_oversized_remote_files(&worktree.join(".beads/dolt-remote"))?;
    reject_oversized_sync_history(worktree)?;
    let refresh = run_git_output_in(worktree, &["update-index", "--refresh"])?;
    if !refresh.stderr.is_empty() {
        io::stderr().write_all(&refresh.stderr)?;
    }
    let status = run_git_required_output_in(
        worktree,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    if status.stdout.is_empty() {
        return Ok(());
    }
    run_git_required_in(worktree, &["add", "-A"])?;
    run_git_required_in(worktree, &["commit", "-m", "bd sync", "--quiet"])
}

fn run_required(program: &'static str, args: &[&str]) -> Result<()> {
    run_required_in(Path::new("."), program, args)
}

fn run_required_in(cwd: &Path, program: &'static str, args: &[&str]) -> Result<()> {
    let output = run_output_in(cwd, program, args)?;
    status_to_result(program, &output)
}

fn run_required_output(program: &'static str, args: &[&str]) -> Result<Output> {
    run_required_output_in(Path::new("."), program, args)
}

fn run_required_output_in(cwd: &Path, program: &'static str, args: &[&str]) -> Result<Output> {
    let output = run_output_in(cwd, program, args)?;
    required_output_result(program, output)
}

fn run_output(program: &'static str, args: &[&str]) -> Result<Output> {
    run_output_in(Path::new("."), program, args)
}

fn run_output_in(cwd: &Path, program: &'static str, args: &[&str]) -> Result<Output> {
    Ok(process_command_in(cwd, program, args).output()?)
}

fn run_git_required(args: &[&str]) -> Result<()> {
    run_git_required_in(Path::new("."), args)
}

fn run_git_required_in(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = run_git_output_in(cwd, args)?;
    status_to_result("git", &output)
}

fn run_git_required_output_in(cwd: &Path, args: &[&str]) -> Result<Output> {
    let output = run_git_output_in(cwd, args)?;
    required_output_result("git", output)
}

fn run_git_output(args: &[&str]) -> Result<Output> {
    run_git_output_in(Path::new("."), args)
}

fn run_git_output_in(cwd: &Path, args: &[&str]) -> Result<Output> {
    let mut command = process_command_in(cwd, "git", args);
    Ok(command.env("PREK_ALLOW_NO_CONFIG", "1").output()?)
}

fn process_command_in(cwd: &Path, program: &str, args: &[&str]) -> ProcessCommand {
    let mut command = ProcessCommand::new(program);
    command.args(args);
    command.current_dir(cwd);
    command.stdin(Stdio::null());
    command
}

fn required_output_result(program: &'static str, output: Output) -> Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(command_failed(program, &output))
    }
}

fn status_to_result(program: &'static str, output: &Output) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failed(program, output))
    }
}

fn command_failed(program: &'static str, output: &Output) -> Error {
    let stderr = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    Error::CommandFailed { program, stderr }
}

fn status_to_exit(output: &Output) -> ExitCode {
    if output.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use super::{
        Command, Error, IssueId, MAX_GITHUB_BLOB_BYTES, check_managed_directory,
        is_fast_forward_rejection, origin_remote_url, parse_affected_ids, read_sync_branch,
        reject_oversized_remote_files, snapshot_query_for_ids,
    };

    #[test]
    fn beads_command_parser_accepts_push() {
        assert_eq!(Command::parse("push"), Some(Command::Push));
    }

    #[test]
    fn fast_forward_rejection_matches_common_dolt_messages() {
        assert!(is_fast_forward_rejection(
            b"non-fast-forward update rejected"
        ));
        assert!(!is_fast_forward_rejection(b"authentication failed"));
        assert!(!is_fast_forward_rejection(b"permission denied (publickey)"));
        assert!(!is_fast_forward_rejection(b"access denied"));
    }

    #[test]
    fn snapshot_query_quotes_validated_issue_ids() {
        let ids = vec![
            IssueId::parse("wx-one").unwrap(),
            IssueId::parse("wx-two.3").unwrap(),
        ];
        let query = snapshot_query_for_ids(&ids);
        assert!(query.contains("'wx-one'"));
        assert!(query.contains("'wx-two.3'"));
    }

    #[test]
    fn affected_id_output_rejects_malformed_query_values() {
        assert_eq!(
            parse_affected_ids(b"id\nwx-one\nwx-two.3\n").unwrap(),
            vec![
                IssueId::parse("wx-one").unwrap(),
                IssueId::parse("wx-two.3").unwrap(),
            ]
        );
        assert!(parse_affected_ids(b"id\nmissing_separator\n").is_err());
        assert!(parse_affected_ids(b"id\nwx-'quoted'\n").is_err());
        assert!(parse_affected_ids(b"id\nwx-empty.\n").is_err());
    }

    #[test]
    fn sync_branch_reader_parses_validated_name() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".beads")).unwrap();
        fs::write(
            root.path().join(".beads/config.yaml"),
            "sync-branch: \"team/beads\"\n",
        )
        .unwrap();

        assert_eq!(
            read_sync_branch(root.path()).unwrap().as_str(),
            "team/beads"
        );
    }

    #[test]
    fn sync_branch_reader_rejects_invalid_name() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".beads")).unwrap();
        fs::write(
            root.path().join(".beads/config.yaml"),
            "sync-branch: \"../outside\"\n",
        )
        .unwrap();

        assert!(read_sync_branch(root.path()).is_err());
    }

    #[test]
    fn managed_directory_rejects_paths_outside_git_root() {
        let root = tempfile::tempdir().unwrap();
        let git_dir = root.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        for path in [
            git_dir.clone(),
            root.path().join("unrelated"),
            git_dir.join("../unrelated"),
        ] {
            assert!(matches!(
                check_managed_directory(&git_dir, &path),
                Err(Error::UnsafeWorktreePath { .. })
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn managed_directory_rejects_dangling_symlink_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let git_dir = root.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), git_dir.join("beads-worktrees"))
            .unwrap();
        assert!(matches!(
            check_managed_directory(&git_dir, &git_dir.join("beads-worktrees/beads")),
            Err(Error::UnsafeWorktreePath { .. })
        ));
    }

    #[test]
    fn remote_file_size_check_accepts_the_exact_github_limit() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("boundary.darc");
        fs::File::create(archive)
            .unwrap()
            .set_len(MAX_GITHUB_BLOB_BYTES)
            .unwrap();
        reject_oversized_remote_files(root.path()).unwrap();
    }

    #[test]
    fn remote_file_size_check_rejects_oversized_nested_files() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("oldgen");
        fs::create_dir(&nested).unwrap();
        let archive = nested.join("oversized.darc");
        fs::File::create(&archive)
            .unwrap()
            .set_len(MAX_GITHUB_BLOB_BYTES + 1)
            .unwrap();
        let error = reject_oversized_remote_files(root.path()).unwrap_err();
        assert!(matches!(error, Error::OversizedRemoteFile { .. }));
        assert!(error.to_string().contains("oldgen/oversized.darc"));
        assert_eq!(
            fs::metadata(archive).unwrap().len(),
            MAX_GITHUB_BLOB_BYTES + 1
        );
    }

    #[test]
    fn remote_file_size_check_allows_an_absent_remote() {
        let root = tempfile::tempdir().unwrap();
        reject_oversized_remote_files(&root.path().join("absent")).unwrap();
    }

    #[test]
    fn origin_remote_url_ignores_matching_non_origin_remote() {
        let remote_list = "backup file:///workspace/.beads/dolt-remote\norigin file:///stale";
        assert_eq!(origin_remote_url(remote_list), Some("file:///stale"));
    }
}
