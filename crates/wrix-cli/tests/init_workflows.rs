mod common;

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, mode, public_key, run_command, run_git, setup_committed_repo,
    write_fake_gh, write_logging_ssh_keygen, write_online_success_git, wrix_command_with_path,
};

struct Fixture {
    directory: tempfile::TempDir,
    repo: tempfile::TempDir,
    home: PathBuf,
    gh: PathBuf,
    git: PathBuf,
    keygen: PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let repo = setup_committed_repo("workflow", false)?;
        let home = directory.path().join("home");
        fs::create_dir(&home)?;
        let gh = write_fake_gh(
            &directory.path().join("bin-gh"),
            &directory.path().join("state"),
            &directory.path().join("gh.log"),
        )?;
        let git = write_online_success_git(&directory.path().join("bin-git"))?;
        let keygen = write_logging_ssh_keygen(
            &directory.path().join("bin-keygen"),
            &directory.path().join("keygen.log"),
        )?;
        Ok(Self {
            directory,
            repo,
            home,
            gh,
            git,
            keygen,
        })
    }

    fn command(&self, args: &[&str]) -> TestResult<Command> {
        let mut command =
            wrix_command_with_path(self.repo.path(), &[&self.gh, &self.git, &self.keygen])?;
        command
            .env("HOSTNAME", "test-host")
            .env("HOME", &self.home)
            .env_remove("WRIX_DEPLOY_KEY")
            .env_remove("WRIX_SIGNING_KEY")
            .arg("init")
            .args(args);
        Ok(command)
    }

    fn run(&self, args: &[&str]) -> TestResult<RunResult> {
        run_command(&mut self.command(args)?)
    }

    fn log(&self, name: &str) -> TestResult<String> {
        Ok(fs::read_to_string(self.directory.path().join(name))?)
    }

    fn assert_rejected_unchanged(&self, args: &[&str], expected: &str) -> TestResult {
        let before = tree(self.repo.path())?;
        let home = tree(&self.home)?;
        let result = self.run(args)?;
        assert_failure_with_clean_stdout(&result);
        assert_contains("diagnostic", &result.stderr, expected);
        assert_eq!(tree(self.repo.path())?, before, "repo changed for {args:?}");
        assert_eq!(tree(&self.home)?, home, "home changed for {args:?}");
        assert_eq!(self.log("gh.log")?, "");
        assert_eq!(self.log("keygen.log")?, "");
        Ok(())
    }
}

type Tree = BTreeMap<PathBuf, (u32, Vec<u8>)>;

fn tree(root: &Path) -> TestResult<Tree> {
    fn visit(root: &Path, path: &Path, entries: &mut Tree) -> TestResult {
        let metadata = fs::metadata(path)?;
        entries.insert(
            path.strip_prefix(root)?.to_path_buf(),
            (
                metadata.permissions().mode(),
                if metadata.is_file() {
                    fs::read(path)?
                } else {
                    Vec::new()
                },
            ),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(root, &entry?.path(), entries)?;
            }
        }
        Ok(())
    }
    let mut entries = Tree::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

#[test]
fn workflow_help_is_complete_and_non_mutating() -> TestResult {
    let fixture = Fixture::new()?;
    let before = tree(fixture.repo.path())?;
    for args in [
        vec!["--help"],
        vec!["deploy", "--help"],
        vec!["repo", "--help"],
        vec!["repo", "--deploy", "--help"],
    ] {
        let result = fixture.run(&args)?;
        assert_success_with_clean_stderr(&result);
        for text in [
            "Verify existing setup",
            "registrations only",
            "then verify",
            "before applying",
            "--force",
            "--no-hooks",
            "--offline",
            "without --deploy",
            "does not grant sandbox",
        ] {
            assert_contains("help", &result.stdout, text);
        }
    }
    assert_eq!(tree(fixture.repo.path())?, before);
    assert_eq!(tree(&fixture.home)?.len(), 1);
    assert_eq!(fixture.log("gh.log")?, "");
    assert_eq!(fixture.log("keygen.log")?, "");
    Ok(())
}

#[test]
fn argument_errors_precede_local_and_remote_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["verify"],
        vec!["provision"],
        vec!["--deploy"],
        vec!["--force"],
        vec!["repo", "--force"],
        vec!["deploy", "--deploy"],
        vec!["deploy", "--no-hooks"],
        vec!["repo", "extra"],
        vec!["--key"],
        vec!["repo", "--remote"],
        vec!["--key", "--offline"],
        vec!["repo", "--sign", "--no-sign"],
        vec!["--offline", "repo"],
        vec!["deploy", "--unknown"],
        vec!["repo", "--", "extra"],
    ] {
        fixture.assert_rejected_unchanged(&args, "Usage: wrix init")?;
    }
    Ok(())
}

#[test]
fn workflow_flag_applicability_is_enforced() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.assert_rejected_unchanged(&["deploy", "--no-hooks"], "--no-hooks is only supported")?;
    for args in [vec!["--force"], vec!["repo", "--force"]] {
        fixture.assert_rejected_unchanged(&args, "--force requires")?;
    }
    for args in [
        vec!["--offline", "--no-hooks", "--key", "key"],
        vec!["repo", "--offline", "--no-hooks", "--key", "key"],
    ] {
        let result = fixture.run(&args)?;
        assert_failure_with_clean_stdout(&result);
        assert_not_contains(
            "valid flags",
            &result.stderr,
            "unexpected wrix init argument",
        );
    }
    assert_eq!(fixture.log("keygen.log")?, "");
    assert_eq!(fixture.log("gh.log")?, "");
    Ok(())
}

#[test]
fn offline_provisioning_is_rejected_before_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["deploy", "--offline"],
        vec!["repo", "--deploy", "--offline"],
    ] {
        fixture.assert_rejected_unchanged(&args, "cannot be used with --offline")?;
    }
    Ok(())
}

#[test]
fn offline_policy_rejects_provisioning_before_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.path().join("wrix.toml"),
        "[init]\nonline_verify = false\n",
    )?;
    for args in [vec!["deploy"], vec!["repo", "--deploy"]] {
        fixture.assert_rejected_unchanged(&args, "requires online verification")?;
    }
    Ok(())
}

#[test]
fn workflows_require_existing_repository_and_remote() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec![],
        vec!["deploy"],
        vec!["repo"],
        vec!["repo", "--deploy"],
    ] {
        let empty = tempfile::tempdir()?;
        let mut command = fixture.command(&args)?;
        command.current_dir(empty.path());
        let result = run_command(&mut command)?;
        assert_failure_with_clean_stdout(&result);
        assert_contains(
            "missing worktree",
            &result.stderr,
            "cannot resolve a Git worktree",
        );
        assert_eq!(tree(empty.path())?.len(), 1);
    }
    run_git(fixture.repo.path(), &["remote", "remove", "origin"])?;
    for args in [
        vec![],
        vec!["deploy"],
        vec!["repo"],
        vec!["repo", "--deploy"],
    ] {
        fixture.assert_rejected_unchanged(&args, "remote 'origin' is not set")?;
    }
    Ok(())
}

#[test]
fn unsupported_provisioning_remote_precedes_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    run_git(
        fixture.repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.org:owner/repo.git",
        ],
    )?;
    for args in [vec!["deploy"], vec!["repo", "--deploy"]] {
        fixture.assert_rejected_unchanged(&args, "unsupported URL")?;
    }
    Ok(())
}

#[test]
fn invalid_policy_and_profile_identity_precede_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    for policy in [
        "[wrix.git]\nsign = true\n",
        "[git]\nsign = false\n[wrix.init]\nonline_verify = true\n",
    ] {
        fs::write(fixture.repo.path().join("wrix.toml"), policy)?;
        for args in [
            vec![],
            vec!["deploy"],
            vec!["repo"],
            vec!["repo", "--deploy"],
        ] {
            fixture.assert_rejected_unchanged(&args, "wrix.toml")?;
        }
    }
    fs::remove_file(fixture.repo.path().join("wrix.toml"))?;
    for json in [r#"{"security":false}"#, r#"{"security":{"deploy_key":42}}"#] {
        let profile = fixture.directory.path().join("profile.json");
        fs::write(&profile, json)?;
        let before = tree(fixture.repo.path())?;
        let result = run_command(
            fixture
                .command(&["deploy"])?
                .arg("--profile-config")
                .arg(&profile),
        )?;
        assert_failure_with_clean_stdout(&result);
        assert_eq!(tree(fixture.repo.path())?, before);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            wrix_cli::init::run(Some(&profile), &["deploy".into()], &mut stdout, &mut stderr)?,
            std::process::ExitCode::FAILURE
        );
        assert_contains(
            "profile identity",
            &String::from_utf8(stderr)?,
            "invalid profile config JSON",
        );
    }
    Ok(())
}

#[test]
fn standalone_deploy_only_provisions_keys() -> TestResult {
    let fixture = Fixture::new()?;
    let before = tree(fixture.repo.path())?;
    let mut command = fixture.command(&["deploy", "--key", "key"])?;
    command
        .env("WRIX_PREK_RUNNER", "/missing-runner")
        .env("WRIX_PREK_HOOKS", "/missing-hooks");
    assert_success_with_clean_stderr(&run_command(&mut command)?);
    assert_eq!(tree(fixture.repo.path())?, before);
    assert!(fixture.home.join(".ssh/deploy_keys/key").is_file());
    assert_contains("write access", &fixture.log("gh.log")?, "read_only=false");
    Ok(())
}

#[test]
fn both_provisioning_forms_pair_requested_signing_keys() -> TestResult {
    for args in [
        vec!["deploy", "--sign", "--key", "key"],
        vec!["repo", "--deploy", "--sign", "--no-hooks", "--key", "key"],
    ] {
        let fixture = Fixture::new()?;
        assert_success_with_clean_stderr(&fixture.run(&args)?);
        let deploy = fixture.home.join(".ssh/deploy_keys/key");
        let signing = fixture.home.join(".ssh/deploy_keys/key-signing");
        assert_ne!(public_key(&deploy)?, public_key(&signing)?);
        for key in [&deploy, &signing] {
            assert_eq!(mode(key)?, 0o600);
            assert_contains("ed25519", &public_key(key)?, "ssh-ed25519 ");
            let output = Command::new("ssh-keygen")
                .args(["-y", "-P", "", "-f"])
                .arg(key)
                .output()?;
            assert!(output.status.success(), "key requires a passphrase");
        }
        assert_eq!(mode(&fixture.home.join(".ssh"))?, 0o700);
        assert_eq!(mode(&fixture.home.join(".ssh/deploy_keys"))?, 0o700);
        let log = fixture.log("gh.log")?;
        assert_contains(
            "deploy registration",
            &log,
            "POST repos/example/workflow/keys",
        );
        assert_contains("write access", &log, "read_only=false");
        assert_contains(
            "account signing registration",
            &log,
            "POST user/ssh_signing_keys",
        );
    }
    Ok(())
}

#[test]
fn provisioning_does_not_infer_or_write_grants() -> TestResult {
    for args in [
        vec!["deploy", "--key", "key"],
        vec!["repo", "--deploy", "--no-hooks", "--key", "key"],
    ] {
        let fixture = Fixture::new()?;
        assert_success_with_clean_stderr(&fixture.run(&args)?);
        assert!(!fixture.repo.path().join("wrix.toml").exists());
        assert!(!fixture.home.join(".ssh/deploy_keys/key-signing").exists());
        assert_not_contains(
            "signing disabled",
            &fixture.log("gh.log")?,
            "ssh_signing_keys",
        );
        let grants = wrix_core::repository_policy::read(fixture.repo.path())?.git;
        assert!(!grants.deploy_enabled());
        assert!(!grants.sign_enabled());
    }
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.path().join("wrix.toml"),
        "[git]\ndeploy = true\n",
    )?;
    fixture.assert_rejected_unchanged(&["--offline", "--key", "key"], "wrix init")?;
    assert!(!fixture.home.join(".ssh").exists());
    Ok(())
}

#[test]
fn standalone_provisioning_is_idempotent() -> TestResult {
    let fixture = Fixture::new()?;
    let args = ["deploy", "--sign", "--key", "key"];
    let repo = tree(fixture.repo.path())?;
    assert_success_with_clean_stderr(&fixture.run(&args)?);
    let home = tree(&fixture.home)?;
    fs::write(fixture.directory.path().join("gh.log"), "")?;
    fs::write(fixture.directory.path().join("keygen.log"), "")?;
    assert_success_with_clean_stderr(&fixture.run(&args)?);
    assert_eq!(tree(fixture.home.as_path())?, home);
    assert_eq!(tree(fixture.repo.path())?, repo);
    assert_not_contains("reuse", &fixture.log("gh.log")?, "POST");
    assert_not_contains("reuse", &fixture.log("gh.log")?, "DELETE");
    assert_not_contains("reuse", &fixture.log("keygen.log")?, "-t\ned25519");
    Ok(())
}

#[test]
fn composed_repo_orders_provision_apply_and_verify() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )?;
    let before = tree(fixture.repo.path())?;
    let conflict = fixture.home.join(".ssh/deploy_keys/key");
    fs::create_dir_all(conflict.parent().unwrap())?;
    fs::write(&conflict, "not a key")?;
    let result = fixture.run(&["repo", "--deploy", "--key", "key"])?;
    assert_failure_with_clean_stdout(&result);
    assert_eq!(
        tree(fixture.repo.path())?,
        before,
        "provision failure applied repo state"
    );
    fs::remove_dir_all(fixture.home.join(".ssh"))?;
    let result = fixture.run(&["repo", "--deploy", "--key", "key", "--no-hooks"])?;
    assert_success_with_clean_stderr(&result);
    assert!(fixture.repo.path().join(".git/wrix/git-ssh").is_file());
    run_git(fixture.repo.path(), &["config", "core.sshCommand", "false"])?;
    let result = fixture.run(&["--offline", "--no-hooks", "--key", "key"])?;
    assert_failure_with_clean_stdout(&result);
    assert_contains("verification runs", &result.stderr, "core.sshCommand");
    let fixture = Fixture::new()?;
    let mut command = fixture.command(&["repo", "--deploy", "--key", "key", "--no-hooks"])?;
    command.env("WRIX_DEPLOY_KEY", fixture.home.join("missing-key"));
    let result = run_command(&mut command)?;
    assert_failure_with_clean_stdout(&result);
    assert!(fixture.home.join(".ssh/deploy_keys/key").is_file());
    assert!(fixture.repo.path().join(".git/wrix/git-ssh").is_file());
    Ok(())
}
