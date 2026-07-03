use std::{
    env,
    error::Error,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use wrix_beads::command::{self, Command};

const CHILD_ENV: &str = "WRIX_BEADS_PUSH_WORKFLOW_CHILD";
const CHILD_TEST: &str = "child_runs_wrix_beads_push";

const BD_FAKE: &str = r#"#!/usr/bin/env bash
set -euo pipefail

log="${WRIX_BEADS_BD_LOG:?}"
state_dir="${WRIX_BEADS_STATE_DIR:?}"
scenario="${WRIX_BEADS_BD_SCENARIO:?}"
root="${WRIX_BEADS_ROOT:?}"
arg1="${1-}"
arg2="${2-}"
arg3="${3-}"
arg4="${4-}"

log_invocation() {
  local arg
  local safe
  {
    printf 'bd'
    for arg in "$@"; do
      safe="${arg//$'\n'/\\n}"
      printf '\t%s' "$safe"
    done
    printf '\n'
  } >> "$log"
}

next_count() {
  local name="$1"
  local file="${state_dir}/${name}.count"
  local count="0"
  if [[ -f "$file" ]]; then
    count="$(< "$file")"
  fi
  count="$((count + 1))"
  printf '%s\n' "$count" > "$file"
  printf '%s\n' "$count"
}

set_auto_export_false() {
  local config="${root}/.beads/config.yaml"
  local tmp="${config}.tmp.$$"
  mkdir -p "$(dirname "$config")"
  if [[ -f "$config" ]]; then
    awk 'BEGIN { seen = 0 } /^export\.auto:/ { if (seen == 0) { print "export.auto: false"; seen = 1 } next } { print } END { if (seen == 0) print "export.auto: false" }' "$config" > "$tmp"
  else
    printf 'export.auto: false\n' > "$tmp"
  fi
  mv "$tmp" "$config"
}

log_invocation "$@"

if [[ "$#" -eq 4 && "$arg1" == "config" && "$arg2" == "set" && "$arg3" == "export.auto" && "$arg4" == "false" ]]; then
  set_auto_export_false
  exit 0
fi

if [[ "$#" -eq 2 && "$arg1" == "dolt" && "$arg2" == "commit" ]]; then
  exit 0
fi

if [[ "$#" -eq 2 && "$arg1" == "dolt" && "$arg2" == "push" ]]; then
  count="$(next_count dolt_push)"
  if [[ "$scenario" == "fallback_diverges" && "$count" == "1" ]]; then
    printf 'non-fast-forward update rejected\n' >&2
    exit 1
  fi
  exit 0
fi

if [[ "$#" -eq 2 && "$arg1" == "dolt" && "$arg2" == "pull" ]]; then
  exit 0
fi

if [[ "$#" -eq 3 && "$arg1" == "dolt" && "$arg2" == "remote" && "$arg3" == "list" ]]; then
  if [[ -n "${WRIX_BEADS_BD_REMOTE_LIST-}" ]]; then
    printf '%s\n' "$WRIX_BEADS_BD_REMOTE_LIST"
  fi
  exit 0
fi

if [[ "$#" -eq 3 && "$arg1" == "sql" && "$arg2" == "--csv" ]]; then
  count="$(next_count sql_csv)"
  if [[ "$scenario" == "fallback_diverges" && "$count" == "1" ]]; then
    printf 'id\nwx-one\n'
  elif [[ "$scenario" == "fallback_diverges" && "$count" == "2" ]]; then
    printf 'id,status,labels\nwx-one,closed,ready\n'
  elif [[ "$scenario" == "fallback_diverges" && "$count" == "3" ]]; then
    printf 'id,status,labels\nwx-one,blocked,ready\n'
  else
    printf 'id\n'
  fi
  exit 0
fi

if [[ "$#" -eq 2 && "$arg1" == "sql" ]]; then
  exit 0
fi

printf 'unexpected bd invocation:' >&2
for arg in "$@"; do
  printf ' <%s>' "$arg" >&2
done
printf '\n' >&2
exit 64
"#;

const GIT_WRAPPER: &str = r#"#!/usr/bin/env bash
set -euo pipefail

log="${WRIX_BEADS_GIT_LOG:?}"
real_git="${WRIX_BEADS_REAL_GIT:?}"

log_invocation() {
  local arg
  local safe
  {
    printf 'PREK_ALLOW_NO_CONFIG=%s\tgit' "${PREK_ALLOW_NO_CONFIG-<unset>}"
    for arg in "$@"; do
      safe="${arg//$'\n'/\\n}"
      printf '\t%s' "$safe"
    done
    printf '\n'
  } >> "$log"
}

log_invocation "$@"
exec "$real_git" "$@"
"#;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    _base: tempfile::TempDir,
    repo: PathBuf,
    fake_bin: PathBuf,
    state_dir: PathBuf,
    bd_log: PathBuf,
    git_log: PathBuf,
}

struct PushOutput {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new(name: &str) -> TestResult<Self> {
        let base = tempfile::Builder::new().prefix(name).tempdir()?;
        let repo = base.path().join("repo");
        let fake_bin = base.path().join("bin");
        let state_dir = base.path().join("state");
        fs::create_dir_all(&repo)?;
        fs::create_dir_all(&fake_bin)?;
        fs::create_dir_all(&state_dir)?;
        let fixture = Self {
            bd_log: base.path().join("bd.log"),
            git_log: base.path().join("git.log"),
            _base: base,
            repo,
            fake_bin,
            state_dir,
        };
        fixture.write_bd_fake()?;
        Ok(fixture)
    }

    fn repo(&self) -> &Path {
        &self.repo
    }

    fn fake_bin(&self) -> &Path {
        &self.fake_bin
    }

    fn beads_worktree(&self) -> PathBuf {
        self.repo.join(".git/beads-worktrees/beads")
    }

    fn worktree_remote_dir(&self) -> PathBuf {
        self.beads_worktree().join(".beads/dolt-remote")
    }

    fn remote_dir(&self) -> PathBuf {
        self.repo.join(".beads/dolt/dolt-remote")
    }

    fn write_bd_fake(&self) -> TestResult {
        write_executable(&self.fake_bin.join("bd"), BD_FAKE)
    }

    fn write_git_wrapper(&self) -> TestResult {
        write_executable(&self.fake_bin.join("git"), GIT_WRAPPER)
    }

    fn bd_lines(&self) -> TestResult<Vec<String>> {
        read_log_lines(&self.bd_log)
    }

    fn git_lines(&self) -> TestResult<Vec<String>> {
        read_log_lines(&self.git_log)
    }
}

#[test]
fn child_runs_wrix_beads_push() -> TestResult {
    if env::var_os(CHILD_ENV).is_none() {
        return Ok(());
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = command::run(Command::Push, &mut stdout, &mut stderr)?;
    let code = if status == ExitCode::SUCCESS {
        "0"
    } else {
        "1"
    };
    fs::write(required_path("WRIX_BEADS_CHILD_STDOUT")?, stdout)?;
    fs::write(required_path("WRIX_BEADS_CHILD_STDERR")?, stderr)?;
    fs::write(required_path("WRIX_BEADS_CHILD_STATUS")?, code)?;
    Ok(())
}

#[test]
fn bd_fake_records_invocations_and_updates_auto_export() -> TestResult {
    let fixture = Fixture::new("bd-fake-contract")?;
    setup_minimal_repo(fixture.repo())?;

    for _ in 0..2 {
        let output = bd_command(&fixture, "success")
            .args(["config", "set", "export.auto", "false"])
            .output()?;
        assert!(
            output.status.success(),
            "fake bd config set failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let config = fs::read_to_string(fixture.repo().join(".beads/config.yaml"))?;
    assert_eq!(config.matches("export.auto: false").count(), 1);
    assert_eq!(
        fixture.bd_lines()?,
        vec![
            String::from("bd\tconfig\tset\texport.auto\tfalse"),
            String::from("bd\tconfig\tset\texport.auto\tfalse"),
        ]
    );
    Ok(())
}

#[test]
fn git_wrapper_records_prek_environment_and_delegates() -> TestResult {
    let fixture = Fixture::new("git-wrapper-contract")?;
    fixture.write_git_wrapper()?;
    let real_git = find_program("git")?;
    let output = ProcessCommand::new(fixture.fake_bin().join("git"))
        .arg("--version")
        .env("WRIX_BEADS_GIT_LOG", &fixture.git_log)
        .env("WRIX_BEADS_REAL_GIT", real_git)
        .env("PREK_ALLOW_NO_CONFIG", "1")
        .output()?;
    assert!(
        output.status.success(),
        "git wrapper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)?.starts_with("git version "));
    assert_eq!(
        fixture.git_lines()?,
        vec![String::from("PREK_ALLOW_NO_CONFIG=1\tgit\t--version")]
    );
    Ok(())
}

#[test]
fn push_precedes_pull() -> TestResult {
    let fixture = Fixture::new("push-precedes-pull")?;
    setup_minimal_repo(fixture.repo())?;

    let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
    })?;

    assert_eq!(output.code, 0, "stderr:\n{}", output.stderr);
    let lines = fixture.bd_lines()?;
    let push_index = command_index(&lines, "bd\tdolt\tpush")?;
    assert!(lines.iter().all(|line| line != "bd\tdolt\tpull"));
    assert!(command_index(&lines, "bd\tdolt\tcommit")? < push_index);
    Ok(())
}

#[test]
fn pull_fallback_preserves_local_intent() -> TestResult {
    let fixture = Fixture::new("pull-fallback-intent")?;
    setup_minimal_repo(fixture.repo())?;

    let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "fallback_diverges");
    })?;

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr
            .contains("diverged from local status/label intent")
    );
    assert!(output.stderr.contains("wx-one"));
    let lines = fixture.bd_lines()?;
    assert_eq!(count_command(&lines, "bd\tdolt\tpush"), 1);
    assert!(command_index(&lines, "bd\tdolt\tpush")? < command_index(&lines, "bd\tdolt\tpull")?);
    Ok(())
}

#[test]
fn disables_auto_export_idempotently() -> TestResult {
    let fixture = Fixture::new("auto-export")?;
    setup_minimal_repo(fixture.repo())?;

    for _ in 0..2 {
        let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
            configure_bd(command, &fixture, "success");
        })?;
        assert_eq!(output.code, 0, "stderr:\n{}", output.stderr);
    }

    let config = fs::read_to_string(fixture.repo().join(".beads/config.yaml"))?;
    assert!(config.contains("export.auto: false"));
    assert_eq!(config.matches("export.auto: false").count(), 1);
    assert!(!fixture.repo().join(".beads/issues.jsonl").exists());
    assert_eq!(
        count_command(&fixture.bd_lines()?, "bd\tconfig\tset\texport.auto\tfalse"),
        2
    );
    Ok(())
}

#[test]
fn repairs_or_temporarily_overrides_dolt_origin() -> TestResult {
    let host = Fixture::new("dolt-origin-host")?;
    setup_repo_with_beads_branch(&host)?;
    fs::create_dir_all(host.worktree_remote_dir())?;
    let host_remote = file_url(&host.worktree_remote_dir());

    let host_output = invoke_push(host.repo(), &[host.fake_bin()], |command| {
        configure_bd(command, &host, "success");
        command.env("WRIX_BEADS_BD_REMOTE_LIST", "origin file:///stale");
    })?;

    assert_eq!(host_output.code, 0, "stderr:\n{}", host_output.stderr);
    assert!(host_output.stderr.contains("repairing Dolt origin remote"));
    let host_lines = host.bd_lines()?;
    assert!(
        host_lines
            .iter()
            .any(|line| line.contains("CALL DOLT_REMOTE('remove', 'origin')"))
    );
    assert!(host_lines.iter().any(|line| line.contains(&format!(
        "CALL DOLT_REMOTE('add', 'origin', '{}')",
        host_remote
    ))));

    let sandbox = Fixture::new("dolt-origin-sandbox")?;
    setup_repo_with_beads_branch(&sandbox)?;
    fs::create_dir_all(sandbox.worktree_remote_dir())?;
    let original_remote = "file:///host-checkout/.git/beads-worktrees/beads/.beads/dolt-remote";
    let sandbox_remote = file_url(&sandbox.worktree_remote_dir());

    let sandbox_output = invoke_push(sandbox.repo(), &[sandbox.fake_bin()], |command| {
        configure_bd(command, &sandbox, "success");
        command.env("IS_SANDBOX", "1");
        command.env(
            "WRIX_BEADS_BD_REMOTE_LIST",
            format!("origin {original_remote}"),
        );
    })?;

    assert_eq!(sandbox_output.code, 0, "stderr:\n{}", sandbox_output.stderr);
    assert!(
        sandbox_output
            .stderr
            .contains("temporarily using sandbox Dolt origin remote")
    );
    let add_lines = sandbox
        .bd_lines()?
        .into_iter()
        .filter(|line| line.contains("CALL DOLT_REMOTE('add', 'origin'"))
        .collect::<Vec<_>>();
    assert_eq!(add_lines.len(), 2);
    assert!(add_lines[0].contains(&sandbox_remote));
    assert!(add_lines[1].contains(original_remote));
    Ok(())
}

#[test]
fn loom_inside_is_noop() -> TestResult {
    let fixture = Fixture::new("loom-inside")?;
    fixture.write_git_wrapper()?;
    let real_git = find_program("git")?;
    let not_repo = fixture.repo().join("not-repo");
    fs::create_dir_all(&not_repo)?;

    let output = invoke_push(&not_repo, &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
        command.env("WRIX_BEADS_GIT_LOG", &fixture.git_log);
        command.env("WRIX_BEADS_REAL_GIT", real_git);
        command.env("LOOM_INSIDE", "1");
    })?;

    assert_eq!(output.code, 0);
    assert_eq!(
        output.stderr.trim(),
        "wrix beads push: LOOM_INSIDE set; loom driver owns publish, skipping"
    );
    assert_eq!(fixture.bd_lines()?, Vec::<String>::new());
    assert_eq!(fixture.git_lines()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn missing_repo_fails_before_git_sync() -> TestResult {
    let fixture = Fixture::new("missing-repo")?;
    let not_repo = fixture.repo().join("not-repo");
    fs::create_dir_all(&not_repo)?;

    let output = invoke_push(&not_repo, &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
    })?;

    assert_eq!(output.code, 1);
    assert!(output.stderr.contains("cannot resolve a git repository"));
    assert!(output.stderr.contains(not_repo.to_string_lossy().as_ref()));
    assert!(
        !output
            .stderr
            .contains("fatal: not a git repository: (null)")
    );
    assert_eq!(fixture.bd_lines()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn pre_pull_cleanup_uses_canonical_dirty_detection() -> TestResult {
    let fixture = Fixture::new("pre-pull-cleanup")?;
    setup_repo_with_beads_branch(&fixture)?;
    let worktree = fixture.beads_worktree();
    fs::write(worktree.join(".beads/.keep"), "modified\n")?;
    fs::write(worktree.join(".beads/interrupted.json"), "leftover\n")?;

    let before = git_stdout(fixture.repo(), &["rev-parse", "origin/beads"])?;
    let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
    })?;

    assert_eq!(output.code, 0, "stderr:\n{}", output.stderr);
    assert!(output.stdout.contains("wrix beads push: synced to GitHub"));
    assert_eq!(
        git_stdout(
            &worktree,
            &["status", "--porcelain", "--untracked-files=normal"]
        )?,
        ""
    );
    assert_eq!(
        git_stdout(&worktree, &["log", "-1", "--pretty=%s"])?,
        "bd sync"
    );
    let committed_paths = git_stdout(
        &worktree,
        &["show", "--name-only", "--pretty=format:", "HEAD"],
    )?;
    assert!(committed_paths.contains(".beads/.keep"));
    assert!(committed_paths.contains(".beads/interrupted.json"));
    assert_ne!(
        before,
        git_stdout(fixture.repo(), &["rev-parse", "origin/beads"])?
    );
    Ok(())
}

#[test]
fn recovers_orphaned_worktree_relative_to_root() -> TestResult {
    let fixture = Fixture::new("orphaned-worktree")?;
    setup_repo_with_beads_branch(&fixture)?;
    let before = git_stdout(fixture.repo(), &["rev-parse", "origin/beads"])?;
    fs::remove_dir_all(fixture.repo().join(".git/worktrees/beads"))?;
    fs::create_dir_all(fixture.remote_dir())?;
    fs::write(fixture.remote_dir().join("db.txt"), "remote data\n")?;

    let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
    })?;

    assert_eq!(output.code, 0, "stderr:\n{}", output.stderr);
    assert!(output.stdout.contains("wrix beads push: synced to GitHub"));
    assert!(
        !output
            .stderr
            .contains("fatal: not a git repository: (null)")
    );
    assert_eq!(
        git_stdout(
            &fixture.beads_worktree(),
            &["rev-parse", "--is-inside-work-tree"]
        )?,
        "true"
    );
    assert!(
        fs::read_to_string(fixture.beads_worktree().join(".git"))?.contains(
            &fixture
                .repo()
                .join(".git/worktrees/beads")
                .display()
                .to_string()
        )
    );
    assert_ne!(
        before,
        git_stdout(fixture.repo(), &["rev-parse", "origin/beads"])?
    );
    Ok(())
}

#[test]
fn git_sync_invocations_skip_prek() -> TestResult {
    let fixture = Fixture::new("git-sync-prek")?;
    setup_repo_with_beads_branch(&fixture)?;
    run_git(
        fixture.repo(),
        &[
            "worktree",
            "remove",
            fixture.beads_worktree().to_string_lossy().as_ref(),
            "--force",
        ],
    )?;
    fixture.write_git_wrapper()?;
    let real_git = find_program("git")?;

    let output = invoke_push(fixture.repo(), &[fixture.fake_bin()], |command| {
        configure_bd(command, &fixture, "success");
        command.env("WRIX_BEADS_GIT_LOG", &fixture.git_log);
        command.env("WRIX_BEADS_REAL_GIT", real_git);
    })?;

    assert_eq!(output.code, 0, "stderr:\n{}", output.stderr);
    assert!(!output.stderr.contains("No prek.toml"));
    let lines = fixture.git_lines()?;
    let sync_lines = lines
        .iter()
        .filter(|line| !line.ends_with("\tgit\trev-parse\t--show-toplevel"))
        .collect::<Vec<_>>();
    assert!(!sync_lines.is_empty());
    assert!(
        sync_lines
            .iter()
            .all(|line| line.starts_with("PREK_ALLOW_NO_CONFIG=1\tgit")),
        "git log:\n{}",
        lines.join("\n")
    );
    assert!(
        sync_lines
            .iter()
            .any(|line| line.contains("\tworktree\tadd\t"))
    );
    Ok(())
}

fn setup_minimal_repo(root: &Path) -> TestResult {
    run_git(root, &["init", "-q", "-b", "main"])?;
    configure_git_identity(root)?;
    write_beads_config(root, true)
}

fn setup_repo_with_beads_branch(fixture: &Fixture) -> TestResult {
    let origin = fixture.repo().with_file_name("origin.git");
    let origin_text = origin.to_string_lossy().into_owned();
    let worktree_text = fixture.beads_worktree().to_string_lossy().into_owned();

    run_git(fixture.repo(), &["init", "-q", "-b", "main"])?;
    configure_git_identity(fixture.repo())?;
    run_git(fixture.repo(), &["init", "--bare", "-q", &origin_text])?;
    fs::write(fixture.repo().join("README.md"), "main\n")?;
    run_git(fixture.repo(), &["add", "README.md"])?;
    run_git(fixture.repo(), &["commit", "-qm", "initial"])?;
    run_git(fixture.repo(), &["remote", "add", "origin", &origin_text])?;
    run_git(fixture.repo(), &["push", "-u", "origin", "main", "--quiet"])?;

    run_git(fixture.repo(), &["switch", "-c", "beads", "--quiet"])?;
    fs::create_dir_all(fixture.repo().join(".beads"))?;
    fs::write(fixture.repo().join(".beads/.keep"), "keep\n")?;
    run_git(fixture.repo(), &["add", ".beads/.keep"])?;
    run_git(fixture.repo(), &["commit", "-qm", "beads initial"])?;
    run_git(
        fixture.repo(),
        &["push", "-u", "origin", "beads", "--quiet"],
    )?;
    run_git(fixture.repo(), &["switch", "main", "--quiet"])?;
    write_beads_config(fixture.repo(), true)?;
    run_git(
        fixture.repo(),
        &["worktree", "add", &worktree_text, "beads", "--quiet"],
    )
}

fn configure_git_identity(root: &Path) -> TestResult {
    run_git(root, &["config", "user.name", "Wrix Test"])?;
    run_git(root, &["config", "user.email", "wrix-test@example.invalid"])
}

fn write_beads_config(root: &Path, auto_export: bool) -> TestResult {
    let value = if auto_export { "true" } else { "false" };
    fs::create_dir_all(root.join(".beads"))?;
    fs::write(
        root.join(".beads/config.yaml"),
        format!(
            "issue-prefix: \"wx\"\nsync-branch: \"beads\"\nsync:\n  mode: dolt-native\nexport.auto: {value}\n"
        ),
    )?;
    Ok(())
}

fn invoke_push(
    cwd: &Path,
    extra_paths: &[&Path],
    configure: impl FnOnce(&mut ProcessCommand),
) -> TestResult<PushOutput> {
    let output_dir = tempfile::Builder::new().prefix("push-output").tempdir()?;
    let stdout_path = output_dir.path().join("stdout");
    let stderr_path = output_dir.path().join("stderr");
    let status_path = output_dir.path().join("status");
    let mut command = ProcessCommand::new(env::current_exe()?);
    command
        .arg("--exact")
        .arg(CHILD_TEST)
        .arg("--nocapture")
        .current_dir(cwd)
        .env(CHILD_ENV, "1")
        .env("WRIX_BEADS_CHILD_STDOUT", &stdout_path)
        .env("WRIX_BEADS_CHILD_STDERR", &stderr_path)
        .env("WRIX_BEADS_CHILD_STATUS", &status_path)
        .env("PATH", path_with_binary_dirs(extra_paths)?)
        .env_remove("LOOM_INSIDE")
        .env_remove("IS_SANDBOX");
    configure(&mut command);
    let harness = command.output()?;
    assert!(
        harness.status.success(),
        "child harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&harness.stdout),
        String::from_utf8_lossy(&harness.stderr)
    );
    let code_text = fs::read_to_string(status_path)?;
    Ok(PushOutput {
        code: code_text.trim().parse()?,
        stdout: fs::read_to_string(stdout_path)?,
        stderr: fs::read_to_string(stderr_path)?,
    })
}

fn configure_bd(command: &mut ProcessCommand, fixture: &Fixture, scenario: &str) {
    command
        .env("WRIX_BEADS_BD_LOG", &fixture.bd_log)
        .env("WRIX_BEADS_STATE_DIR", &fixture.state_dir)
        .env("WRIX_BEADS_BD_SCENARIO", scenario)
        .env("WRIX_BEADS_ROOT", fixture.repo());
}

fn bd_command(fixture: &Fixture, scenario: &str) -> ProcessCommand {
    let mut command = ProcessCommand::new(fixture.fake_bin().join("bd"));
    configure_bd(&mut command, fixture, scenario);
    command
}

fn run_git(cwd: &Path, args: &[&str]) -> TestResult {
    let output = git_command(cwd, args).output()?;
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn git_stdout(cwd: &Path, args: &[&str]) -> TestResult<String> {
    let output = git_command(cwd, args).output()?;
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn git_command(cwd: &Path, args: &[&str]) -> ProcessCommand {
    let mut command = ProcessCommand::new("git");
    command
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn path_with_binary_dirs(extra_paths: &[&Path]) -> TestResult<OsString> {
    let mut paths = extra_paths
        .iter()
        .map(|path| (*path).to_path_buf())
        .collect::<Vec<_>>();
    let Some(current) = env::var_os("PATH") else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "PATH is not set").into());
    };
    paths.extend(env::split_paths(&current));
    Ok(env::join_paths(paths)?)
}

fn write_executable(path: &Path, content: &str) -> TestResult {
    fs::write(path, content)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn read_log_lines(path: &Path) -> TestResult<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}

fn required_path(name: &str) -> TestResult<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name} is not set")).into())
}

fn command_index(lines: &[String], command: &str) -> TestResult<usize> {
    lines
        .iter()
        .position(|line| line == command)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing command {command}; log:\n{}", lines.join("\n")),
            )
            .into()
        })
}

fn count_command(lines: &[String], command: &str) -> usize {
    lines.iter().filter(|line| line.as_str() == command).count()
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn find_program(name: &str) -> TestResult<PathBuf> {
    let Some(path) = env::var_os("PATH") else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "PATH is not set").into());
    };
    for directory in env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{name} not found on PATH")).into())
}
