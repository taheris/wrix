use std::{
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Output, Stdio},
};

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

pub const HELP: &str =
    "Manage beads workflows.\n\nUsage: wrix beads <command>\n\nCommands:\n  push\n";

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(HELP.as_bytes())
}

pub fn run(
    command: Command,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    match command {
        Command::Push => push(stdout, stderr),
    }
}

fn push(stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<ExitCode> {
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
    branch: String,
    worktree: PathBuf,
    worktree_remote_dir: PathBuf,
    remote_dir: PathBuf,
}

impl Context {
    fn load(current_dir: &Path) -> io::Result<Option<Self>> {
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
        let branch = read_sync_branch(&root)?;
        let worktree = root.join(".git/beads-worktrees").join(&branch);
        let worktree_remote_dir = worktree.join(".beads/dolt-remote");
        let remote_dir = root.join(".beads/dolt/dolt-remote");
        Ok(Some(Self {
            root,
            branch,
            worktree,
            worktree_remote_dir,
            remote_dir,
        }))
    }
}

fn peel_beads_worktree(root: &Path) -> Option<PathBuf> {
    let text = root.to_string_lossy();
    text.find("/.git/beads-worktrees/")
        .map(|index| PathBuf::from(&text[..index]))
}

fn read_sync_branch(root: &Path) -> io::Result<String> {
    let config_path = root.join(".beads/config.yaml");
    if !config_path.exists() {
        return Ok(String::from("beads"));
    }
    let content = fs::read_to_string(config_path)?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("sync-branch:") {
            let branch = rest.trim().trim_matches('"');
            if !branch.is_empty() {
                return Ok(branch.to_owned());
            }
        }
    }
    Ok(String::from("beads"))
}

fn prepare_dolt_origin_remote(
    context: &Context,
    stderr: &mut impl Write,
) -> io::Result<DoltRemoteOverride> {
    if !context.worktree_remote_dir.is_dir() {
        return Ok(DoltRemoteOverride::inactive());
    }

    let remote = format!("file://{}", context.worktree_remote_dir.display());
    let list = run_output("bd", &["dolt", "remote", "list"])?;
    let origin = if list.status.success() {
        let text = String::from_utf8_lossy(&list.stdout);
        origin_remote_url(&text).map(ToOwned::to_owned)
    } else {
        None
    };
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
    ) -> io::Result<Self> {
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

    fn restore(self) -> io::Result<()> {
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

fn replace_dolt_origin(existing: Option<&str>, remote: &str) -> io::Result<()> {
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

fn disable_auto_export() -> io::Result<()> {
    run_required("bd", &["config", "set", "export.auto", "false"])
}

fn sync_dolt_remote(stderr: &mut impl Write) -> io::Result<ExitCode> {
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
    if !is_fast_forward_rejection(&push.stderr) {
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

fn pull_with_intent_protection(stderr: &mut impl Write) -> io::Result<ExitCode> {
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
            affected_ids.join(" ")
        )?;
        return Ok(ExitCode::FAILURE);
    }
    let push = run_output("bd", &["dolt", "push"])?;
    Ok(status_to_exit(&push))
}

fn query_affected_ids() -> io::Result<Vec<String>> {
    let output = run_required_output("bd", &["sql", "--csv", AFFECTED_IDS_SQL])?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

const AFFECTED_IDS_SQL: &str = "\n    SELECT DISTINCT id FROM (\n      SELECT to_id AS id\n      FROM dolt_commit_diff_issues\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n        AND (from_status IS NULL OR from_status <> to_status)\n      UNION\n      SELECT to_issue_id AS id\n      FROM dolt_commit_diff_labels\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n      UNION\n      SELECT from_issue_id AS id\n      FROM dolt_commit_diff_labels\n      WHERE to_commit = 'HEAD' AND from_commit = 'remotes/origin/main'\n    ) AS touched\n    WHERE id IS NOT NULL\n";

fn snapshot_query_for_ids(ids: &[String]) -> String {
    let in_list = ids
        .iter()
        .map(|id| sql_quote(id))
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
) -> io::Result<ExitCode> {
    if !ensure_beads_worktree(context, stderr)? {
        return Ok(ExitCode::SUCCESS);
    }
    repair_worktree_pointers(context)?;
    commit_dirty_worktree(&context.worktree)?;
    run_git_required_in(&context.worktree, &["pull", "--rebase", "--quiet"])?;

    if context.remote_dir.is_dir() {
        let source = format!("{}/", context.remote_dir.display());
        let destination = format!("{}/", context.worktree.join(".beads/dolt-remote").display());
        run_required("rsync", &["-a", "--delete", &source, &destination])?;
    }

    commit_dirty_worktree(&context.worktree)?;
    run_git_required_in(
        &context.worktree,
        &["push", "-u", "origin", &context.branch, "--quiet"],
    )?;
    writeln!(stdout, "wrix beads push: synced to GitHub")?;
    Ok(ExitCode::SUCCESS)
}

fn ensure_beads_worktree(context: &Context, stderr: &mut impl Write) -> io::Result<bool> {
    if context.worktree.is_dir()
        && run_git_output_in(&context.worktree, &["rev-parse", "--is-inside-work-tree"])?
            .status
            .success()
    {
        return Ok(true);
    }
    if context.worktree.is_dir() {
        run_git_required(&["worktree", "prune"])?;
        fs::remove_dir_all(&context.worktree)?;
    }
    if run_git_output(&["rev-parse", "--verify", &context.branch])?
        .status
        .success()
    {
        let worktree = context.worktree_text();
        run_git_required(&["worktree", "add", &worktree, &context.branch, "--quiet"])?;
    } else {
        let origin_branch = format!("origin/{}", context.branch);
        if run_git_output(&["rev-parse", "--verify", &origin_branch])?
            .status
            .success()
        {
            let worktree = context.worktree_text();
            run_git_required(&["worktree", "add", &worktree, &origin_branch, "--quiet"])?;
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

fn repair_worktree_pointers(context: &Context) -> io::Result<()> {
    let dotgit = context.worktree.join(".git");
    let admin = context.root.join(".git/worktrees").join(&context.branch);
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

fn commit_dirty_worktree(worktree: &Path) -> io::Result<()> {
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

fn run_required(program: &str, args: &[&str]) -> io::Result<()> {
    run_required_in(Path::new("."), program, args)
}

fn run_required_in(cwd: &Path, program: &str, args: &[&str]) -> io::Result<()> {
    let output = run_output_in(cwd, program, args)?;
    status_to_result(&output)
}

fn run_required_output(program: &str, args: &[&str]) -> io::Result<Output> {
    run_required_output_in(Path::new("."), program, args)
}

fn run_required_output_in(cwd: &Path, program: &str, args: &[&str]) -> io::Result<Output> {
    let output = run_output_in(cwd, program, args)?;
    required_output_result(program, output)
}

fn run_output(program: &str, args: &[&str]) -> io::Result<Output> {
    run_output_in(Path::new("."), program, args)
}

fn run_output_in(cwd: &Path, program: &str, args: &[&str]) -> io::Result<Output> {
    process_command_in(cwd, program, args).output()
}

fn run_git_required(args: &[&str]) -> io::Result<()> {
    run_git_required_in(Path::new("."), args)
}

fn run_git_required_in(cwd: &Path, args: &[&str]) -> io::Result<()> {
    let output = run_git_output_in(cwd, args)?;
    status_to_result(&output)
}

fn run_git_required_output_in(cwd: &Path, args: &[&str]) -> io::Result<Output> {
    let output = run_git_output_in(cwd, args)?;
    required_output_result("git", output)
}

fn run_git_output(args: &[&str]) -> io::Result<Output> {
    run_git_output_in(Path::new("."), args)
}

fn run_git_output_in(cwd: &Path, args: &[&str]) -> io::Result<Output> {
    let mut command = process_command_in(cwd, "git", args);
    command.env("PREK_ALLOW_NO_CONFIG", "1").output()
}

fn process_command_in(cwd: &Path, program: &str, args: &[&str]) -> ProcessCommand {
    let mut command = ProcessCommand::new(program);
    command.args(args);
    command.current_dir(cwd);
    command.stdin(Stdio::null());
    command
}

fn required_output_result(program: &str, output: Output) -> io::Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(io::Error::other(format!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn status_to_result(output: &Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
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
    use super::{Command, is_fast_forward_rejection, origin_remote_url, snapshot_query_for_ids};

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
    fn snapshot_query_quotes_issue_ids() {
        let ids = vec![String::from("wx-one"), String::from("wx-'two")];
        let query = snapshot_query_for_ids(&ids);
        assert!(query.contains("'wx-one'"));
        assert!(query.contains("'wx-''two'"));
    }

    #[test]
    fn origin_remote_url_ignores_matching_non_origin_remote() {
        let remote_list = "backup file:///workspace/.beads/dolt-remote\norigin file:///stale";
        assert_eq!(origin_remote_url(remote_list), Some("file:///stale"));
    }
}
