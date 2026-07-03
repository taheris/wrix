#![expect(
    dead_code,
    reason = "common integration-test helpers are shared across test targets that each use a subset"
)]

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub struct RunResult {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

pub fn wrix_command(cwd: &Path) -> TestResult<Command> {
    wrix_command_with_path(cwd, &[])
}

pub fn wrix_command_with_path(cwd: &Path, extra_paths: &[&Path]) -> TestResult<Command> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
    command.current_dir(cwd);
    set_isolated_env(&mut command, extra_paths)?;
    Ok(command)
}

pub fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    set_isolated_git_env(&mut command);
    command
}

pub fn git_command_with_path(
    cwd: &Path,
    args: &[&str],
    extra_paths: &[&Path],
) -> TestResult<Command> {
    let mut command = git_command(cwd, args);
    command.env("PATH", path_with_binary_dir(extra_paths)?);
    Ok(command)
}

pub fn run_command(command: &mut Command) -> TestResult<RunResult> {
    let output = command.output()?;
    Ok(RunResult {
        status: output.status,
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

pub fn run_git(cwd: &Path, args: &[&str]) -> TestResult {
    let result = run_command(&mut git_command(cwd, args))?;
    assert!(
        result.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        result.stdout,
        result.stderr,
    );
    Ok(())
}

pub fn git_stdout(cwd: &Path, args: &[&str]) -> TestResult<String> {
    let result = run_command(&mut git_command(cwd, args))?;
    assert!(
        result.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        result.stdout,
        result.stderr,
    );
    Ok(result.stdout.trim().to_owned())
}

pub fn setup_repo(name: &str) -> TestResult<tempfile::TempDir> {
    let repo = tempfile::Builder::new().prefix(name).tempdir()?;
    run_git(repo.path(), &["init", "-q"])?;
    run_git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            &format!("git@github.com:example/{name}.git"),
        ],
    )?;
    Ok(repo)
}

pub fn setup_committed_repo(name: &str, with_prek_config: bool) -> TestResult<tempfile::TempDir> {
    let repo = setup_repo(name)?;
    run_git(repo.path(), &["config", "user.name", "Wrix Test"])?;
    run_git(
        repo.path(),
        &["config", "user.email", "wrix-test@example.invalid"],
    )?;
    fs::write(repo.path().join("README.md"), "initial\n")?;
    let mut paths = vec!["README.md"];
    if with_prek_config {
        fs::write(repo.path().join(".pre-commit-config.yaml"), "repos:\n")?;
        paths.push(".pre-commit-config.yaml");
    }
    run_git(repo.path(), &["add", paths[0]])?;
    if with_prek_config {
        run_git(repo.path(), &["add", paths[1]])?;
    }
    run_git(repo.path(), &["commit", "-qm", "initial"])?;
    Ok(repo)
}

pub fn common_git_dir(repo: &Path) -> TestResult<PathBuf> {
    let value = git_stdout(repo, &["rev-parse", "--git-common-dir"])?;
    let common_dir = PathBuf::from(&value);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        PathBuf::from(git_stdout(repo, &["rev-parse", "--show-toplevel"])?).join(common_dir)
    };
    Ok(common_dir.canonicalize()?)
}

pub fn write_empty_key(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    fs::write(path, "")?;
    set_mode(path, 0o600)
}

pub fn write_ed25519_key(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    let result = run_command(
        Command::new("ssh-keygen")
            .arg("-q")
            .arg("-t")
            .arg("ed25519")
            .arg("-N")
            .arg("")
            .arg("-f")
            .arg(path),
    )?;
    assert!(
        result.status.success(),
        "ssh-keygen failed\nstdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr,
    );
    set_mode(path, 0o600)
}

pub fn public_key(path: &Path) -> TestResult<String> {
    let result = run_command(Command::new("ssh-keygen").arg("-y").arg("-f").arg(path))?;
    assert!(
        result.status.success(),
        "ssh-keygen -y failed\nstdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr,
    );
    Ok(result.stdout.trim().to_owned())
}

pub fn write_prek_hooks(path: &Path) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    for name in [
        "pre-commit",
        "pre-push",
        "prepare-commit-msg",
        "post-checkout",
        "post-merge",
    ] {
        let hook = path.join(name);
        fs::write(&hook, "#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n")?;
        set_mode(&hook, 0o700)?;
    }
    Ok(path.canonicalize()?)
}

pub fn write_online_success_git(path: &Path) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    let real_git = command_path("git")?;
    let script = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\n\nfor arg in \"$@\"; do\n  if [[ \"$arg\" == \"ls-remote\" ]]; then\n    printf '%s\\tHEAD\\n' \"0123456789012345678901234567890123456789\"\n    exit 0\n  fi\ndone\nexec {} \"$@\"\n",
        shell_single_quote(&real_git.display().to_string()),
    );
    let git = path.join("git");
    fs::write(&git, script)?;
    set_mode(&git, 0o700)?;
    Ok(path.to_path_buf())
}

pub fn write_capturing_git(
    path: &Path,
    mode_file: &Path,
    capture_dir: &Path,
) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    let real_git = command_path("git")?;
    let script = r#"#!/usr/bin/env bash
set -euo pipefail

capture_dir=@CAPTURE_DIR@
mode_file=@MODE_FILE@
for arg in "$@"; do
  if [[ "$arg" == "ls-remote" ]]; then
    mkdir -p "$capture_dir"
    pwd -P >"$capture_dir/cwd"
    printf '%s\n' "$@" >"$capture_dir/args"
    env | sort >"$capture_dir/env"
    mode="$(<"$mode_file")"
    case "$mode" in
      success)
        printf '%s\tHEAD\n' "0123456789012345678901234567890123456789"
        exit 0
        ;;
      host-key)
        printf 'Host key verification failed.\n' >&2
        exit 128
        ;;
      auth)
        printf 'Permission denied (publickey).\nfatal: Could not read from remote repository.\n' >&2
        exit 128
        ;;
      fail-if-online)
        printf 'unexpected online verification\n' >&2
        exit 99
        ;;
      *)
        printf 'unknown fake git mode: %s\n' "$mode" >&2
        exit 98
        ;;
    esac
  fi
done
exec @REAL_GIT@ "$@"
"#
    .replace(
        "@CAPTURE_DIR@",
        &shell_single_quote(&capture_dir.display().to_string()),
    )
    .replace(
        "@MODE_FILE@",
        &shell_single_quote(&mode_file.display().to_string()),
    )
    .replace(
        "@REAL_GIT@",
        &shell_single_quote(&real_git.display().to_string()),
    );
    let git = path.join("git");
    fs::write(&git, script)?;
    set_mode(&git, 0o700)?;
    Ok(path.to_path_buf())
}

pub fn write_fake_gh(path: &Path, state_dir: &Path, log_file: &Path) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    fs::create_dir_all(state_dir)?;
    if !log_file.exists() {
        fs::write(log_file, "")?;
    }
    let script = r#"#!/usr/bin/env bash
set -euo pipefail

state_dir=@STATE_DIR@
log_file=@LOG_FILE@

get_field() {
  local prefix="$1"
  shift
  local field
  for field in "$@"; do
    if [[ "$field" == "$prefix"* ]]; then
      printf '%s\n' "${field#"$prefix"}"
      return 0
    fi
  done
  printf 'missing field: %s\n' "$prefix" >&2
  return 1
}

write_deploy_list() {
  if [[ ! -f "$state_dir/deploy_title" ]]; then
    printf '[]\n'
    return 0
  fi
  local id title key read_only
  id="$(<"$state_dir/deploy_id")"
  title="$(<"$state_dir/deploy_title")"
  key="$(<"$state_dir/deploy_key")"
  read_only="$(<"$state_dir/deploy_read_only")"
  printf '[{"id":%s,"title":"%s","key":"%s","read_only":%s}]\n' "$id" "$title" "$key" "$read_only"
}

write_signing_list() {
  if [[ ! -f "$state_dir/signing_title" ]]; then
    printf '[]\n'
    return 0
  fi
  local id title key
  id="$(<"$state_dir/signing_id")"
  title="$(<"$state_dir/signing_title")"
  key="$(<"$state_dir/signing_key")"
  printf '[{"id":%s,"title":"%s","key":"%s"}]\n' "$id" "$title" "$key"
}

if [[ "${1:-}" != "api" ]]; then
  printf 'unsupported gh command\n' >&2
  exit 2
fi
shift
method="GET"
endpoint=""
fields=()
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --method)
      method="$2"
      shift 2
      ;;
    --raw-field | --field)
      fields+=("$2")
      shift 2
      ;;
    --*)
      printf 'unsupported gh api option: %s\n' "$1" >&2
      exit 2
      ;;
    *)
      if [[ -z "$endpoint" ]]; then
        endpoint="$1"
      fi
      shift
      ;;
  esac
done
printf '%s %s %s\n' "$method" "$endpoint" "${fields[*]}" >>"$log_file"

if [[ "$method" == "GET" && "$endpoint" == repos/example/*/keys\?per_page=100 ]]; then
  write_deploy_list
elif [[ "$method" == "GET" && "$endpoint" == "user/ssh_signing_keys?per_page=100" ]]; then
  write_signing_list
elif [[ "$method" == "POST" && "$endpoint" == repos/example/*/keys ]]; then
  title="$(get_field "title=" "${fields[@]}")"
  key="$(get_field "key=" "${fields[@]}")"
  read_only="$(get_field "read_only=" "${fields[@]}")"
  if [[ "$read_only" != "false" ]]; then
    printf 'deploy key was not requested with write access\n' >&2
    exit 3
  fi
  printf '1\n' >"$state_dir/deploy_id"
  printf '%s\n' "$title" >"$state_dir/deploy_title"
  printf '%s\n' "$key" >"$state_dir/deploy_key"
  printf '%s\n' "$read_only" >"$state_dir/deploy_read_only"
  printf '{"id":1}\n'
elif [[ "$method" == "POST" && "$endpoint" == "user/ssh_signing_keys" ]]; then
  title="$(get_field "title=" "${fields[@]}")"
  key="$(get_field "key=" "${fields[@]}")"
  printf '2\n' >"$state_dir/signing_id"
  printf '%s\n' "$title" >"$state_dir/signing_title"
  printf '%s\n' "$key" >"$state_dir/signing_key"
  printf '{"id":2}\n'
elif [[ "$method" == "DELETE" && "$endpoint" == repos/example/*/keys/* ]]; then
  rm -f "$state_dir/deploy_id" "$state_dir/deploy_title" "$state_dir/deploy_key" "$state_dir/deploy_read_only"
  printf '{}\n'
elif [[ "$method" == "DELETE" && "$endpoint" == user/ssh_signing_keys/* ]]; then
  rm -f "$state_dir/signing_id" "$state_dir/signing_title" "$state_dir/signing_key"
  printf '{}\n'
else
  printf 'unsupported gh api call: %s %s\n' "$method" "$endpoint" >&2
  exit 2
fi
"#
    .replace(
        "@STATE_DIR@",
        &shell_single_quote(&state_dir.display().to_string()),
    )
    .replace(
        "@LOG_FILE@",
        &shell_single_quote(&log_file.display().to_string()),
    );
    let gh = path.join("gh");
    fs::write(&gh, script)?;
    set_mode(&gh, 0o700)?;
    Ok(path.to_path_buf())
}

pub fn write_logging_ssh_keygen(path: &Path, log_file: &Path) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    if !log_file.exists() {
        fs::write(log_file, "")?;
    }
    let real_ssh_keygen = command_path("ssh-keygen")?;
    let script = r#"#!/usr/bin/env bash
set -euo pipefail

log_file=@LOG_FILE@
{
  printf '__CALL__\n'
  printf '%s\n' "$@"
} >>"$log_file"
exec @REAL_SSH_KEYGEN@ "$@"
"#
    .replace(
        "@LOG_FILE@",
        &shell_single_quote(&log_file.display().to_string()),
    )
    .replace(
        "@REAL_SSH_KEYGEN@",
        &shell_single_quote(&real_ssh_keygen.display().to_string()),
    );
    let ssh_keygen = path.join("ssh-keygen");
    fs::write(&ssh_keygen, script)?;
    set_mode(&ssh_keygen, 0o700)?;
    Ok(path.to_path_buf())
}

pub fn write_fake_ssh(path: &Path) -> TestResult<PathBuf> {
    fs::create_dir_all(path)?;
    let ssh = path.join("ssh");
    fs::write(
        &ssh,
        "#!/usr/bin/env bash\nset -euo pipefail\n: \"$WRIX_TEST_CAPTURE\"\nprintf '%s\\n' \"$@\" >\"$WRIX_TEST_CAPTURE\"\n",
    )?;
    set_mode(&ssh, 0o700)?;
    Ok(path.to_path_buf())
}

pub fn mode(path: &Path) -> TestResult<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

pub fn binary_dir() -> TestResult<PathBuf> {
    let path = Path::new(env!("CARGO_BIN_EXE_wrix"));
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "wrix binary has no parent directory",
        )
    })?;
    Ok(parent.to_path_buf())
}

pub fn assert_success_with_clean_stderr(result: &RunResult) {
    assert!(result.status.success(), "stderr: {}", result.stderr);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {}",
        result.stderr,
    );
}

pub fn assert_failure_with_clean_stdout(result: &RunResult) {
    assert!(!result.status.success(), "command unexpectedly succeeded");
    assert!(
        result.stdout.is_empty(),
        "unexpected stdout: {}",
        result.stdout,
    );
}

pub fn assert_contains(label: &str, haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "{label}: missing {needle:?} in {haystack:?}",
    );
}

pub fn assert_not_contains(label: &str, haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "{label}: unexpected {needle:?} in {haystack:?}",
    );
}

pub fn set_mode(path: &Path, mode: u32) -> TestResult {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn set_isolated_env(command: &mut Command, extra_paths: &[&Path]) -> TestResult {
    set_isolated_git_env(command);
    command.env("PATH", path_with_binary_dir(extra_paths)?);
    for name in [
        "HOME",
        "HOSTNAME",
        "WRIX_DEPLOY_KEY",
        "WRIX_SIGNING_KEY",
        "WRIX_PREK_HOOKS",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "SSH_AUTH_SOCK",
    ] {
        command.env_remove(name);
    }
    Ok(())
}

fn set_isolated_git_env(command: &mut Command) {
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
}

fn path_with_binary_dir(extra_paths: &[&Path]) -> TestResult<OsString> {
    let mut paths = extra_paths
        .iter()
        .map(|path| (*path).to_path_buf())
        .collect::<Vec<_>>();
    paths.push(binary_dir()?);
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    Ok(env::join_paths(paths)?)
}

fn command_path(name: &str) -> TestResult<PathBuf> {
    let path = env::var_os("PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PATH is not set"))?;
    for directory in env::split_paths(&path) {
        let candidate = directory.join(name);
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{name} not found on PATH")).into())
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && (metadata.permissions().mode() & 0o111) != 0)
}

fn shell_single_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}
