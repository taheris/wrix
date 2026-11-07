#![cfg(test)]

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    temporary: tempfile::TempDir,
    repo: PathBuf,
    formatter: PathBuf,
    git_env: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        fs::create_dir(&repo).unwrap();
        fs::write(
            temporary.path().join("unavailable-cache"),
            "A file cannot serve as a cache directory.\n",
        )
        .unwrap();
        let root = PathBuf::from(env::var_os("WRIX_TEST_REPOSITORY").unwrap());
        let formatter = PathBuf::from(env::var_os("WRIX_TEST_FORMATTER").unwrap());
        fs::copy(
            root.join(".pre-commit-config.yaml"),
            repo.join(".pre-commit-config.yaml"),
        )
        .unwrap();
        fs::write(repo.join("flake.nix"), "{}\n").unwrap();
        fs::write(repo.join("README.md"), "# Fixture\n").unwrap();
        let result = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
            .unwrap();
        assert!(result.status.success());
        let git_env = String::from_utf8(result.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let fixture = Self {
            temporary,
            repo,
            formatter,
            git_env,
        };
        for arguments in [["init", "-q"].as_slice(), ["add", "."].as_slice()] {
            let result = fixture.invoke("git", arguments);
            assert!(result.status.success(), "{result:?}");
        }
        fixture
    }

    fn invoke(&self, program: impl AsRef<std::ffi::OsStr>, arguments: &[&str]) -> Output {
        let mut command = Command::new(program);
        command.current_dir(&self.repo).args(arguments);
        for name in &self.git_env {
            command.env_remove(name);
        }
        let paths = env::join_paths(
            std::iter::once(self.formatter.parent().unwrap().to_path_buf())
                .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        command
            .env("PATH", paths)
            .env("PREK_HOME", self.temporary.path().join("prek"))
            .env("PREK_COLOR", "never")
            .env(
                "XDG_CACHE_HOME",
                self.temporary.path().join("unavailable-cache"),
            )
            .env("TREEFMT_NO_CACHE", "false")
            .env("TREEFMT_CI", "false");
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("formatter call timed out in {}", self.repo.display());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.wait_with_output().unwrap()
    }

    fn hook(&self) -> Output {
        self.invoke(
            "prek",
            &[
                "run",
                "treefmt",
                "--stage",
                "pre-commit",
                "--files",
                "README.md",
                "--no-progress",
            ],
        )
    }

    fn readme(&self) -> String {
        fs::read_to_string(self.repo.join("README.md")).unwrap()
    }
}

#[test]
#[ignore = "requires the actual Nix formatter and packaged prek"]
fn fixture_prevents_cached_formatter_execution() {
    let fixture = Fixture::new();
    let result = fixture.invoke(&fixture.formatter, &["--fail-on-change", "README.md"]);
    assert!(!result.status.success(), "{result:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(text.to_lowercase().contains("cache"), "{text}");
}

#[test]
#[ignore = "requires the actual Nix formatter and packaged prek"]
fn clean_file_passes_hook_without_cache_access() {
    let fixture = Fixture::new();
    let result = fixture.hook();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(fixture.readme(), "# Fixture\n");
}

#[test]
#[ignore = "requires the actual Nix formatter and packaged prek"]
fn formatting_change_still_fails_hook_without_cache_access() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join("README.md"), "#   Fixture\n").unwrap();
    let result = fixture.hook();
    assert!(!result.status.success(), "{result:?}");
    assert_eq!(fixture.readme(), "# Fixture\n");
    let rerun = fixture.hook();
    assert!(rerun.status.success(), "{rerun:?}");
}
