mod common;

use std::{
    env,
    fmt::Write as _,
    fs,
    io::Write,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use common::{
    RunResult, TestResult, git_stdout, run_command, run_git, set_mode, setup_committed_repo,
    write_empty_key, wrix_command,
};

const STAGES: [&str; 5] = [
    "pre-commit",
    "pre-push",
    "post-checkout",
    "post-merge",
    "prepare-commit-msg",
];

struct Fixture {
    repo: tempfile::TempDir,
    work: tempfile::TempDir,
    tools: PathBuf,
    system: String,
    runtime: &'static common::prek::Runtime,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let runtime = common::prek::runtime()?;
        let work = tempfile::tempdir()?;
        let tools = work.path().join("minimal path");
        fs::create_dir(&tools)?;
        for name in ["bash", "git", "uname"] {
            let path = env::var_os("PATH").ok_or("test PATH is missing")?;
            let executable = env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|path| path.is_file())
                .ok_or_else(|| format!("bootstrap tool {name} is missing"))?;
            symlink(executable, tools.join(name))?;
        }
        let repo = setup_committed_repo("prek-runtime", false)?;
        fs::write(
            repo.path().join("probe"),
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"$1\" >>probe.log\n[[ ! -f fail ]]\n",
        )?;
        set_mode(&repo.path().join("probe"), 0o755)?;
        let mut config = String::from("repos:\n  - repo: local\n    hooks:\n");
        for stage in STAGES {
            write!(
                config,
                "      - id: {stage}\n        name: {stage}\n        entry: ./probe {stage}\n        language: system\n        stages: [{stage}]\n        always_run: true\n        pass_filenames: false\n"
            )?;
        }
        fs::write(repo.path().join(".pre-commit-config.yaml"), config)?;
        run_git(repo.path(), &["add", "."])?;
        run_git(repo.path(), &["commit", "-qm", "configure probes"])?;
        let arch = env::consts::ARCH;
        let os = if env::consts::OS == "macos" {
            "darwin"
        } else {
            "linux"
        };
        Ok(Self {
            repo,
            work,
            tools,
            system: format!("{arch}-{os}"),
            runtime,
        })
    }

    fn command(&self, executable: &Path, cwd: &Path, context: &str) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(cwd)
            .env("PATH", &self.tools)
            .env("HOME", self.work.path())
            .env("PREK_HOME", self.work.path().join("prek-cache"))
            .env("WRIX_PREK_CONTEXT", context)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("WRIX_PREK_RUNNER");
        command
    }

    fn bind(&self, cwd: &Path, context: &str) -> TestResult {
        let result = run_command(
            self.command(&self.runtime.runner, cwd, context)
                .arg("--bind"),
        )?;
        assert!(result.status.success(), "bind: {}", result.stderr);
        Ok(())
    }

    fn key(&self, context: &str) -> String {
        format!("wrix.prek-{context}-{}.runner", self.system)
    }

    fn hook(&self, stage: &str, cwd: &Path, context: &str) -> TestResult<RunResult> {
        let head = git_stdout(cwd, &["rev-parse", "HEAD"])?;
        let mut command = self.command(&self.runtime.hooks.join(stage), cwd, context);
        command.env("WRIX_TEST_STAGE", stage);
        match stage {
            "pre-push" => {
                command.args(["origin", "example"]);
            }
            "post-checkout" => {
                command.args([&head, &head, "1"]);
            }
            "post-merge" => {
                command.arg("0");
            }
            "prepare-commit-msg" => {
                let message = self.work.path().join("message");
                fs::write(&message, "test message\n")?;
                command.arg(message).arg("message");
            }
            _ => {}
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if stage == "pre-push" {
            writeln!(
                child.stdin.take().ok_or("piped stdin is missing")?,
                "refs/heads/main {head} refs/heads/main {}",
                "0".repeat(40)
            )?;
        } else {
            drop(child.stdin.take());
        }
        let output = child.wait_with_output()?;
        Ok(RunResult {
            status: output.status,
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }

    fn init(&self) -> TestResult<RunResult> {
        let key = self.work.path().join("deploy-key");
        write_empty_key(&key)?;
        run_command(
            wrix_command(self.repo.path())?
                .args(["init", "--offline", "--no-sign", "--key", "test-key"])
                .env("HOME", self.work.path())
                .env("WRIX_DEPLOY_KEY", key)
                .env("WRIX_PREK_HOOKS", &self.runtime.hooks)
                .env("WRIX_PREK_RUNNER", &self.runtime.runner)
                .env("WRIX_PREK_CONTEXT", "host"),
        )
    }
}

#[test]
fn every_packaged_stage_runs_without_devshell_path() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.bind(fixture.repo.path(), "host")?;
    assert!(!fixture.tools.join("wrix-prek").exists());
    assert!(!fixture.tools.join("prek").exists());
    for stage in STAGES {
        let result = fixture.hook(stage, fixture.repo.path(), "host")?;
        assert!(
            result.status.success(),
            "{stage}: {} {}",
            result.stdout,
            result.stderr
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.repo.path().join("probe.log"))?,
        STAGES.join("\n") + "\n"
    );
    assert!(fixture.repo.path().join(".wrix/push-verified").exists());
    Ok(())
}

#[test]
fn git_commit_and_push_use_installed_bundle_with_minimal_path() -> TestResult {
    let fixture = Fixture::new()?;
    assert!(fixture.init()?.status.success());
    let git = fixture.tools.join("git");
    let result = run_command(fixture.command(&git, fixture.repo.path(), "host").args([
        "commit",
        "--allow-empty",
        "-qm",
        "hooked commit",
    ]))?;
    assert!(
        result.status.success(),
        "commit: {} {}",
        result.stdout,
        result.stderr
    );
    let remote = fixture.work.path().join("remote.git");
    run_git(
        fixture.repo.path(),
        &["init", "--bare", "-q", remote.to_str().expect("UTF-8 path")],
    )?;
    let result = run_command(
        fixture
            .command(&git, fixture.repo.path(), "host")
            .arg("push")
            .arg(&remote)
            .arg("HEAD:refs/heads/main"),
    )?;
    assert!(
        result.status.success(),
        "push: {} {}",
        result.stdout,
        result.stderr
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.path().join("probe.log"))?,
        "pre-commit\nprepare-commit-msg\npre-push\n"
    );
    Ok(())
}

#[test]
fn every_packaged_stage_propagates_hook_failure() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.bind(fixture.repo.path(), "host")?;
    fs::write(fixture.repo.path().join("fail"), "")?;
    for stage in STAGES {
        let result = fixture.hook(stage, fixture.repo.path(), "host")?;
        assert!(!result.status.success(), "{stage} silently passed");
        assert!(
            result.stdout.contains("Failed"),
            "{stage} did not reach failing probe: {} {}",
            result.stdout,
            result.stderr
        );
    }
    assert!(!fixture.repo.path().join(".wrix/push-verified").exists());
    Ok(())
}

#[test]
fn packaged_launcher_initializes_binding_without_runner_on_path() -> TestResult {
    let fixture = Fixture::new()?;
    let packaged = common::prek::packaged_wrix()?;
    symlink(common::command_path("ssh")?, fixture.tools.join("ssh"))?;
    let key = fixture.work.path().join("deploy-key");
    write_empty_key(&key)?;
    let result = run_command(
        fixture
            .command(&packaged, fixture.repo.path(), "host")
            .args(["init", "--offline", "--no-sign", "--key", "test-key"])
            .env("WRIX_DEPLOY_KEY", key)
            .env_remove("WRIX_PREK_HOOKS"),
    )?;
    assert!(result.status.success(), "packaged init: {}", result.stderr);
    assert!(
        fixture
            .hook("post-merge", fixture.repo.path(), "host")?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn init_repairs_stale_runner_and_hook_path() -> TestResult {
    let fixture = Fixture::new()?;
    run_git(
        fixture.repo.path(),
        &["config", &fixture.key("host"), "/missing/runner"],
    )?;
    run_git(
        fixture.repo.path(),
        &["config", "core.hooksPath", "/missing/hooks"],
    )?;
    let result = fixture.init()?;
    assert!(result.status.success(), "init: {}", result.stderr);
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", "core.hooksPath"])?,
        fixture.runtime.hooks.display().to_string()
    );
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", &fixture.key("host")])?,
        fixture.runtime.runner.display().to_string()
    );
    assert!(
        fixture
            .hook("post-merge", fixture.repo.path(), "host")?
            .status
            .success()
    );
    let config = fs::read(fixture.repo.path().join(".git/config"))?;
    assert!(fixture.init()?.status.success());
    assert_eq!(fs::read(fixture.repo.path().join(".git/config"))?, config);
    Ok(())
}

#[test]
fn devshell_entry_repairs_binding_even_when_hook_path_is_current() -> TestResult {
    let fixture = Fixture::new()?;
    run_git(
        fixture.repo.path(),
        &[
            "config",
            "core.hooksPath",
            fixture.runtime.hooks.to_str().expect("UTF-8 hooks"),
        ],
    )?;
    run_git(
        fixture.repo.path(),
        &["config", &fixture.key("host"), "/stale/runner"],
    )?;
    let hook = common::prek::devshell_hook()?;
    let result = run_command(
        fixture
            .command(&fixture.tools.join("bash"), fixture.repo.path(), "host")
            .arg("-c")
            .arg(hook),
    )?;
    assert!(result.status.success(), "shell entry: {}", result.stderr);
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", &fixture.key("host")])?,
        fixture.runtime.runner.display().to_string()
    );
    assert!(
        fixture
            .hook("post-merge", fixture.repo.path(), "host")?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn synthetic_foreign_platform_does_not_use_native_binding() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.bind(fixture.repo.path(), "host")?;
    let uname = fixture.tools.join("uname");
    fs::remove_file(&uname)?;
    let (os, system) = if env::consts::OS == "macos" {
        ("Linux", "aarch64-linux")
    } else {
        ("Darwin", "aarch64-darwin")
    };
    fs::write(
        &uname,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ncase \"$1\" in -s) printf '%s\\n' '{os}';; -m) printf '%s\\n' arm64;; esac\n"
        ),
    )?;
    set_mode(&uname, 0o755)?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(!result.status.success());
    assert!(
        result
            .stderr
            .contains(&format!("wrix.prek-host-{system}.runner")),
        "{}",
        result.stderr
    );
    assert!(!fixture.repo.path().join("probe.log").exists());
    Ok(())
}

#[test]
fn missing_bootstrap_dependency_reports_actionable_failure() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.bind(fixture.repo.path(), "host")?;
    fs::remove_file(fixture.tools.join("uname"))?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(!result.status.success());
    assert!(result.stderr.contains("hook bootstrap needs uname on PATH"));
    assert!(result.stderr.contains("repair hooks"));
    Ok(())
}

#[test]
fn broken_runtime_reports_actionable_failure() -> TestResult {
    let fixture = Fixture::new()?;
    let broken = &fixture.runtime.broken_runner;
    run_git(
        fixture.repo.path(),
        &[
            "config",
            &fixture.key("host"),
            broken.to_str().expect("UTF-8 executable"),
        ],
    )?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(!result.status.success());
    assert!(
        result
            .stderr
            .contains("could not resolve its packaged runtime")
    );
    assert!(result.stderr.contains("packaged hook runtime dependency"));
    assert!(result.stderr.contains("repair hooks"));
    Ok(())
}

#[test]
fn binding_rejects_missing_packaged_dependency_before_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    let config = fs::read(fixture.repo.path().join(".git/config"))?;
    let result = run_command(
        fixture
            .command(&fixture.runtime.broken_runner, fixture.repo.path(), "host")
            .arg("--bind"),
    )?;
    assert!(!result.status.success());
    assert!(result.stderr.contains("packaged hook runtime dependency"));
    assert_eq!(fs::read(fixture.repo.path().join(".git/config"))?, config);
    Ok(())
}

#[test]
fn missing_binding_fails_with_repair_instructions() -> TestResult {
    let fixture = Fixture::new()?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(!result.status.success());
    assert!(
        result.stderr.contains("no hook runner binding"),
        "{}",
        result.stderr
    );
    assert!(result.stderr.contains("reload") && result.stderr.contains("wrix init"));
    assert!(!fixture.repo.path().join("probe.log").exists());
    Ok(())
}

#[test]
fn stale_binding_does_not_fall_back_to_ambient_runner() -> TestResult {
    let fixture = Fixture::new()?;
    symlink(&fixture.runtime.runner, fixture.tools.join("wrix-prek"))?;
    run_git(
        fixture.repo.path(),
        &["config", &fixture.key("host"), "/missing/runner"],
    )?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(!result.status.success());
    assert!(result.stderr.contains("missing or invalid hook runner"));
    Ok(())
}

#[test]
fn legacy_installation_can_use_packaged_runner_on_path() -> TestResult {
    let fixture = Fixture::new()?;
    symlink(&fixture.runtime.runner, fixture.tools.join("wrix-prek"))?;
    let result = fixture.hook("post-merge", fixture.repo.path(), "host")?;
    assert!(result.status.success(), "{}", result.stderr);
    Ok(())
}

#[test]
fn worker_binding_preserves_host_and_foreign_platform_bindings() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.bind(fixture.repo.path(), "host")?;
    let host = git_stdout(fixture.repo.path(), &["config", &fixture.key("host")])?;
    let foreign = "wrix.prek-host-aarch64-darwin.runner";
    let foreign = if foreign == fixture.key("host") {
        "wrix.prek-host-x86_64-linux.runner"
    } else {
        foreign
    };
    run_git(
        fixture.repo.path(),
        &["config", foreign, "/foreign/nix/store/wrix-prek"],
    )?;
    fixture.bind(fixture.repo.path(), "container")?;
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", &fixture.key("host")])?,
        host
    );
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", foreign])?,
        "/foreign/nix/store/wrix-prek"
    );
    assert!(
        fixture
            .hook("post-merge", fixture.repo.path(), "container")?
            .status
            .success()
    );
    run_git(
        fixture.repo.path(),
        &[
            "config",
            &fixture.key("container"),
            "/missing/container/runner",
        ],
    )?;
    assert!(
        fixture
            .hook("post-merge", fixture.repo.path(), "host")?
            .status
            .success()
    );
    assert!(
        !fixture
            .hook("post-merge", fixture.repo.path(), "container")?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn linked_worktree_uses_common_binding_but_clone_needs_its_own() -> TestResult {
    let fixture = Fixture::new()?;
    let linked = fixture.work.path().join("linked worktree");
    run_git(
        fixture.repo.path(),
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            linked.to_str().expect("UTF-8 path"),
        ],
    )?;
    fixture.bind(&linked, "host")?;
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", &fixture.key("host")])?,
        fixture.runtime.runner.display().to_string()
    );
    assert!(
        fixture
            .hook("post-merge", &linked, "host")?
            .status
            .success()
    );
    let clone = fixture.work.path().join("independent clone");
    run_git(
        fixture.repo.path(),
        &["clone", "-q", ".", clone.to_str().expect("UTF-8 path")],
    )?;
    assert!(!fixture.hook("post-merge", &clone, "host")?.status.success());
    fixture.bind(&clone, "host")?;
    assert!(fixture.hook("post-merge", &clone, "host")?.status.success());
    Ok(())
}
