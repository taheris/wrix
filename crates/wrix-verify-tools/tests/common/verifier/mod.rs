use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::Value;

#[derive(Debug)]
pub struct Deadline;

pub fn bounded(command: &mut Command, timeout: Duration) -> Result<Output, Deadline> {
    // Regular files drain large discovery output without pipe-buffer deadlocks.
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = command
        .process_group(0)
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
            match kill_process_group(pid, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => panic!("cannot stop owned fixture process group: {error}"),
            }
            child.wait().unwrap();
            stdout.seek(SeekFrom::Start(0)).unwrap();
            stderr.seek(SeekFrom::Start(0)).unwrap();
            let mut diagnostic = String::new();
            stdout.read_to_string(&mut diagnostic).unwrap();
            eprintln!("timed-out fixture stdout:\n{diagnostic}");
            diagnostic.clear();
            stderr.read_to_string(&mut diagnostic).unwrap();
            eprintln!("timed-out fixture stderr:\n{diagnostic}");
            return Err(Deadline);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout).unwrap();
    stderr.read_to_end(&mut output.stderr).unwrap();
    Ok(output)
}

pub fn capture(command: &mut Command) -> Output {
    bounded(command, Duration::from_secs(120))
        .expect("verifier fixture command exceeded its deadline")
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}
pub fn combined(result: &Output) -> String {
    format!("{}{}", text(&result.stdout), text(&result.stderr))
}
pub fn records(bytes: &[u8]) -> Vec<Value> {
    text(bytes)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
pub fn quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    format!(
        "'{}'",
        value.as_ref().to_str().unwrap().replace('\'', "'\"'\"'")
    )
}
fn seed_fetcher_cache(snapshot: &Path, cache: &Path) {
    let destination = cache.join("nix");
    fs::create_dir_all(&destination).unwrap();
    let git = snapshot.join("gitv3");
    if git.is_dir() {
        // Only the suite-owned Git object cache is shared; never caller state.
        std::os::unix::fs::symlink(git, destination.join("gitv3")).unwrap();
    }
    for entry in fs::read_dir(snapshot).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let version = name
            .to_str()
            .and_then(|name| name.strip_prefix("fetcher-cache-v"))
            .and_then(|name| name.strip_suffix(".sqlite"));
        if entry.file_type().unwrap().is_file()
            && version.is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            fs::copy(entry.path(), destination.join(name)).unwrap();
        }
    }
}

pub fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
pub fn git_locals() -> Vec<String> {
    let result = capture(Command::new("git").args(["rev-parse", "--local-env-vars"]));
    assert!(result.status.success(), "{result:?}");
    text(&result.stdout).lines().map(str::to_owned).collect()
}

pub struct Sandbox {
    _temporary: tempfile::TempDir,
    pub root: PathBuf,
    pub environment: HashMap<OsString, OsString>,
}

impl Sandbox {
    pub fn new(prefix: &str) -> Self {
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut environment: HashMap<_, _> = env::vars_os().collect();
        environment.insert("REPO_ROOT".into(), root.as_os_str().to_owned());
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            let directory = root.join(directory);
            fs::create_dir_all(&directory).unwrap();
            environment.insert(key.into(), directory.into_os_string());
        }
        if let Some(snapshot) = env::var_os("WRIX_TEST_FETCHER_CACHE_SNAPSHOT") {
            seed_fetcher_cache(Path::new(&snapshot), &root.join("cache"));
        }
        fs::write(root.join("calls"), "").unwrap();
        Self {
            _temporary: temporary,
            root,
            environment,
        }
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(&self.environment);
        command
    }

    pub fn read(&self, path: &str) -> String {
        fs::read_to_string(self.root.join(path)).unwrap()
    }
    pub fn write(&self, path: &str, contents: &str) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, contents).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_home_configuration_and_state_do_not_share_caller_or_neighbor_roots() {
        let caller_home = env::var_os("HOME");
        let first = Sandbox::new("verifier-private-roots-");
        let second = Sandbox::new("verifier-private-roots-");
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            let key = std::ffi::OsStr::new(key);
            assert_eq!(
                first.environment.get(key),
                Some(&first.root.join(directory).into_os_string())
            );
            assert_ne!(first.environment.get(key), second.environment.get(key));
        }
        assert_eq!(env::var_os("HOME"), caller_home);
        assert!(!first.root.join("cache/wrix").exists());
        assert!(!first.root.join("state/wrix").exists());
    }

    #[test]
    fn fetcher_seed_keeps_databases_private_and_excludes_admission_state() {
        let snapshot = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let database = snapshot.path().join("fetcher-cache-v4.sqlite");
        fs::write(&database, "pinned source metadata").unwrap();
        for name in [
            "admission.sqlite",
            "fetcher-cache-v4.sqlite-wal",
            "fetcher-cache-vx.sqlite",
        ] {
            fs::write(snapshot.path().join(name), "must not be inherited").unwrap();
        }
        std::os::unix::fs::symlink(&database, snapshot.path().join("fetcher-cache-v9.sqlite"))
            .unwrap();
        seed_fetcher_cache(snapshot.path(), cache.path());
        let copied = cache.path().join("nix/fetcher-cache-v4.sqlite");
        assert_eq!(
            fs::read_to_string(&copied).unwrap(),
            "pinned source metadata"
        );
        fs::write(&copied, "private fixture update").unwrap();
        assert_eq!(
            fs::read_to_string(database).unwrap(),
            "pinned source metadata"
        );
        assert_eq!(fs::read_dir(cache.path().join("nix")).unwrap().count(), 1);
        assert!(!cache.path().join("wrix").exists());
        let git = snapshot.path().join("gitv3");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("pinned-object"), "suite-owned source object").unwrap();
        let second = tempfile::tempdir().unwrap();
        seed_fetcher_cache(snapshot.path(), second.path());
        let linked = second.path().join("nix/gitv3");
        assert!(linked.symlink_metadata().unwrap().is_symlink());
        assert_eq!(linked.canonicalize().unwrap(), git.canonicalize().unwrap());
        assert_eq!(
            fs::read_to_string(linked.join("pinned-object")).unwrap(),
            "suite-owned source object"
        );
        assert!(!second.path().join("wrix").exists());
    }

    #[test]
    fn capture_drains_both_streams_beyond_pipe_capacity() {
        let result = capture(Command::new("bash").args(["-c", "for ((i=0; i<10000; i++)); do printf 'stdout-data\\n'; printf 'stderr-data\\n' >&2; done"]));
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 120_000);
        assert_eq!(result.stderr.len(), 120_000);
    }

    #[test]
    fn deadline_stops_the_owned_command_and_its_child() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("escaped");
        let result = bounded(
            Command::new("bash")
                .args(["-c", "(sleep 1; printf escaped > \"$1\") & wait", "fixture"])
                .arg(&marker),
            Duration::from_millis(50),
        );
        assert!(result.is_err());
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists());
    }
}
