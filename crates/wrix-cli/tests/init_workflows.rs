mod common;

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use common::{
    RunResult, TestResult, assert_contains, assert_failure_with_clean_stdout, assert_not_contains,
    assert_success_with_clean_stderr, common_git_dir, git_stdout, mode, public_key, run_command,
    run_git, set_mode, setup_committed_repo, write_capturing_ssh, write_empty_key, write_fake_gh,
    write_logging_ssh_keygen, write_online_success_git, write_tracing_git, wrix_command_with_path,
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

type Tree = BTreeMap<PathBuf, (u32, std::time::SystemTime, i64, i64, Vec<u8>)>;

fn tree(root: &Path) -> TestResult<Tree> {
    fn visit(root: &Path, path: &Path, entries: &mut Tree) -> TestResult {
        let metadata = fs::metadata(path)?;
        entries.insert(
            path.strip_prefix(root)?.to_path_buf(),
            (
                metadata.permissions().mode(),
                metadata.modified()?,
                metadata.ctime(),
                metadata.ctime_nsec(),
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
fn fixture_git_disables_automatic_maintenance_despite_inherited_configuration() -> TestResult {
    const CHILD: &str = "WRIX_TEST_FIXTURE_MAINTENANCE_CHILD";
    const TEST: &str = "fixture_git_disables_automatic_maintenance_despite_inherited_configuration";
    if std::env::var_os(CHILD).is_some() {
        let fixture = Fixture::new()?;
        for (key, expected) in [("maintenance.auto", "false"), ("gc.auto", "0")] {
            assert_eq!(
                git_stdout(fixture.repo.path(), &["config", "--get", key])?,
                expected
            );
        }
        offline_policy_rejects_provisioning_before_mutation()?;
        required_signing_material_is_not_implicitly_provisioned()?;
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let trace = directory.path().join("git-trace.jsonl");
    let result = run_command(
        Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "maintenance.auto")
            .env("GIT_CONFIG_VALUE_0", "true")
            .env("GIT_CONFIG_KEY_1", "gc.auto")
            .env("GIT_CONFIG_VALUE_1", "1")
            .env(
                "GIT_CONFIG_PARAMETERS",
                "'maintenance.auto=true' 'gc.auto=1'",
            )
            .env("GIT_TRACE2_EVENT", &trace),
    )?;
    assert_success_with_clean_stderr(&result);
    let events = fs::read_to_string(trace)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["event"] == "start"
            && event["argv"]
                .as_array()
                .is_some_and(|args| args.iter().any(|arg| arg == "commit"))
    }));
    for event in events
        .iter()
        .filter(|event| event["event"] == "child_start")
    {
        let args = event["argv"].as_array().unwrap();
        assert!(
            !args.iter().any(|arg| arg == "maintenance" || arg == "gc"),
            "fixture launched automatic maintenance: {event}"
        );
    }
    Ok(())
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

#[test]
fn online_verification_uses_invoked_repository_and_strict_helper() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("online-helper", false)?;
    let integration = clone_integration(repo.path())?;
    run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/online-helper.git",
        ],
    )?;
    let integration_config_path = integration.join(".git/config");
    let integration_config = fs::read(&integration_config_path)?;
    let home = fixture.home("online-helper");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fixture.set_mode("success")?;

    assert_ne!(common_git_dir(repo.path())?, common_git_dir(&integration)?);
    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("online output", &result.stdout, "online_verify: true");
    assert_online_capture(
        &fixture.git_capture_dir,
        &fixture.ssh_capture_dir,
        &repo.path().canonicalize()?,
        &common_git_dir(repo.path())?,
        &deploy_key,
        &home,
        "example/online-helper.git",
    )?;
    assert_eq!(fs::read(integration_config_path)?, integration_config);
    let command = git_stdout(repo.path(), &["config", "--get", "core.sshCommand"])?;
    assert_contains("outer helper config", &command, "wrix/git-ssh");
    Ok(())
}

#[test]
fn init_inside_integration_clone_verifies_that_repository() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("integration-helper", false)?;
    let integration = clone_integration(repo.path())?;
    run_git(
        &integration,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:example/integration-helper.git",
        ],
    )?;
    let outer_config_path = repo.path().join(".git/config");
    let outer_config = fs::read(&outer_config_path)?;
    let home = fixture.home("integration-helper");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(&integration, &home, &deploy_key)?;
    fixture.set_mode("success")?;

    let result = fixture.run_init(
        &integration,
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("integration output", &result.stdout, "online_verify: true");
    assert_online_capture(
        &fixture.git_capture_dir,
        &fixture.ssh_capture_dir,
        &integration,
        &common_git_dir(&integration)?,
        &deploy_key,
        &home,
        "example/integration-helper.git",
    )?;
    assert_eq!(fs::read(outer_config_path)?, outer_config);
    let command = git_stdout(&integration, &["config", "--get", "core.sshCommand"])?;
    assert_contains("integration helper config", &command, "wrix/git-ssh");
    Ok(())
}

#[test]
fn unsupported_verification_remote_precedes_network() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("non-github-online", false)?;
    let local_remote = repo.path().join("local-origin.git");
    run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            &local_remote.display().to_string(),
        ],
    )?;
    let config_path = repo.path().join(".git/config");
    let original_config = fs::read(&config_path)?;
    let home = fixture.home("non-github-online");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "non-GitHub online remote",
        &result.stderr,
        "online verification requires a github.com remote",
    );
    assert_eq!(fs::read(config_path)?, original_config);
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn offline_verification_retains_local_checks() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-flag", false)?;
    let home = fixture.home("offline-flag");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains("offline output", &result.stdout, "online_verify: false");
    fixture.assert_no_online_capture();
    assert_offline_local_checks(false)?;
    Ok(())
}

#[test]
fn offline_policy_retains_local_checks() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-config", false)?;
    let home = fixture.home("offline-config");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    fs::write(
        repo.path().join("wrix.toml"),
        "[init]\nonline_verify = false\n",
    )?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--no-sign", "--key", "verify-key"],
    )?;

    assert_success_with_clean_stderr(&result);
    assert_contains(
        "config offline output",
        &result.stdout,
        "online_verify: false",
    );
    fixture.assert_no_online_capture();
    assert_offline_local_checks(true)?;
    Ok(())
}

#[test]
fn offline_verification_rejects_insecure_permissions() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("offline-local-check", false)?;
    let home = fixture.home("offline-local-check");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    set_mode(&deploy_key, 0o644)?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains("offline local permissions", &result.stderr, "deploy key");
    assert_contains(
        "offline local permissions",
        &result.stderr,
        "no group or other permissions",
    );
    fixture.assert_no_online_capture();
    Ok(())
}

#[test]
fn verification_distinguishes_host_keys_and_authorization() -> TestResult {
    let fixture = VerifyFixture::new()?;

    let host_key_repo = setup_committed_repo("host-key-failure", false)?;
    let host_key_home = fixture.home("host-key-failure");
    let host_key = write_deploy_key(&host_key_home, 0o600)?;
    fixture.prepare(host_key_repo.path(), &host_key_home, &host_key)?;
    fixture.set_mode("host-key")?;
    let result = fixture.run_init(
        host_key_repo.path(),
        &host_key_home,
        &host_key,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "host-key failure",
        &result.stderr,
        "online verification failed host-key verification",
    );
    assert_not_contains(
        "host-key failure",
        &result.stderr,
        "authentication or repository authorization failed",
    );

    let auth_repo = setup_committed_repo("auth-failure", false)?;
    let auth_home = fixture.home("auth-failure");
    let auth_key = write_deploy_key(&auth_home, 0o600)?;
    fixture.prepare(auth_repo.path(), &auth_home, &auth_key)?;
    fixture.set_mode("auth")?;
    let result = fixture.run_init(
        auth_repo.path(),
        &auth_home,
        &auth_key,
        &["--no-sign", "--key", "verify-key"],
    )?;
    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "auth failure",
        &result.stderr,
        "authentication or repository authorization failed",
    );
    assert_not_contains(
        "auth failure",
        &result.stderr,
        "failed host-key verification",
    );
    Ok(())
}

#[test]
fn verification_rejects_worktree_transport_override() -> TestResult {
    let fixture = VerifyFixture::new()?;
    let repo = setup_committed_repo("worktree-transport-override", false)?;
    let home = fixture.home("worktree-transport-override");
    let deploy_key = write_deploy_key(&home, 0o600)?;
    fixture.prepare(repo.path(), &home, &deploy_key)?;
    run_git(
        repo.path(),
        &["config", "extensions.worktreeConfig", "true"],
    )?;
    run_git(
        repo.path(),
        &[
            "config",
            "--worktree",
            "core.sshCommand",
            "ssh -o StrictHostKeyChecking=no",
        ],
    )?;
    fixture.set_mode("fail-if-online")?;

    let result = fixture.run_init(
        repo.path(),
        &home,
        &deploy_key,
        &["--offline", "--no-sign", "--key", "verify-key"],
    )?;

    assert_failure_with_clean_stdout(&result);
    assert_contains(
        "worktree override",
        &result.stderr,
        "core.sshCommand does not match the Wrix common-dir trampoline",
    );
    assert_contains(
        "worktree override",
        &result.stderr,
        "StrictHostKeyChecking=no",
    );
    fixture.assert_no_online_capture();
    Ok(())
}

struct VerifyFixture {
    directory: tempfile::TempDir,
    mode_file: PathBuf,
    git_capture_dir: PathBuf,
    ssh_capture_dir: PathBuf,
    tracing_git: PathBuf,
    fake_ssh: PathBuf,
}

impl VerifyFixture {
    fn new() -> TestResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix("wrix-init-verify-fixtures")
            .tempdir()?;
        let mode_file = directory.path().join("ssh-mode");
        let git_capture_dir = directory.path().join("git-capture");
        let ssh_capture_dir = directory.path().join("ssh-capture");
        let tracing_git =
            write_tracing_git(&directory.path().join("tracing-git"), &git_capture_dir)?;
        let fake_ssh = write_capturing_ssh(
            &directory.path().join("fake-ssh"),
            &mode_file,
            &ssh_capture_dir,
        )?;
        Ok(Self {
            directory,
            mode_file,
            git_capture_dir,
            ssh_capture_dir,
            tracing_git,
            fake_ssh,
        })
    }

    fn home(&self, name: &str) -> PathBuf {
        self.directory.path().join(format!("home-{name}"))
    }

    fn set_mode(&self, mode: &str) -> TestResult {
        fs::write(&self.mode_file, format!("{mode}\n"))?;
        for capture_dir in [&self.git_capture_dir, &self.ssh_capture_dir] {
            if capture_dir.exists() {
                fs::remove_dir_all(capture_dir)?;
            }
            fs::create_dir_all(capture_dir)?;
        }
        Ok(())
    }

    fn assert_no_online_capture(&self) {
        assert_absent(&self.git_capture_dir.join("cwd"));
        assert_absent(&self.ssh_capture_dir.join("cwd"));
    }

    fn prepare(&self, repo: &Path, home: &Path, deploy_key: &Path) -> TestResult {
        let mut command = wrix_command_with_path(repo, &[&self.tracing_git, &self.fake_ssh])?;
        let result = run_command(
            command
                .args([
                    "init",
                    "repo",
                    "--offline",
                    "--no-sign",
                    "--key",
                    "verify-key",
                ])
                .env("HOME", home)
                .env("WRIX_DEPLOY_KEY", deploy_key),
        )?;
        assert_success_with_clean_stderr(&result);
        Ok(())
    }

    fn run_init(
        &self,
        repo: &Path,
        home: &Path,
        deploy_key: &Path,
        args: &[&str],
    ) -> TestResult<RunResult> {
        let mut command = wrix_command_with_path(repo, &[&self.tracing_git, &self.fake_ssh])?;
        command
            .arg("init")
            .args(args)
            .env("GIT_SSH_COMMAND", "ambient ssh")
            .env("SSH_AUTH_SOCK", home.join("agent.sock"))
            .env("WRIX_SHOULD_NOT_LEAK", "1")
            .env("HOME", home)
            .env("WRIX_DEPLOY_KEY", deploy_key);
        let repo_before = tree(repo)?;
        let home_before = tree(home)?;
        let result = run_command(&mut command)?;
        assert_eq!(
            tree(repo)?,
            repo_before,
            "verification changed repository state"
        );
        assert_eq!(tree(home)?, home_before, "verification changed credentials");
        Ok(result)
    }
}

fn clone_integration(repo: &Path) -> TestResult<PathBuf> {
    let integration = repo.join(".loom/integration");
    fs::create_dir_all(repo.join(".loom"))?;
    let integration_path = integration.display().to_string();
    run_git(repo, &["clone", "-q", ".", &integration_path])?;
    Ok(integration.canonicalize()?)
}

fn write_deploy_key(home: &Path, mode: u32) -> TestResult<PathBuf> {
    let key = home.join(".ssh/deploy_keys/verify-key");
    write_empty_key(&key)?;
    set_mode(&home.join(".ssh"), 0o700)?;
    set_mode(&home.join(".ssh/deploy_keys"), 0o700)?;
    set_mode(&key, mode)?;
    Ok(key)
}

fn assert_online_capture(
    git_capture_dir: &Path,
    ssh_capture_dir: &Path,
    expected_cwd: &Path,
    common_dir: &Path,
    deploy_key: &Path,
    home: &Path,
    expected_repo: &str,
) -> TestResult {
    let git_cwd = fs::read_to_string(git_capture_dir.join("cwd"))?;
    assert_eq!(git_cwd.trim(), expected_cwd.display().to_string());
    assert_eq!(
        fs::read_to_string(git_capture_dir.join("args"))?,
        format!("ls-remote\ngit@github.com:{expected_repo}\nHEAD\n"),
    );
    let git_env = fs::read_to_string(git_capture_dir.join("env"))?;
    assert_minimal_online_env("online Git env", &git_env, deploy_key, home);

    let ssh_cwd = fs::read_to_string(ssh_capture_dir.join("cwd"))?;
    assert_eq!(ssh_cwd.trim(), expected_cwd.display().to_string());
    let args = fs::read_to_string(ssh_capture_dir.join("args"))?;
    assert_contains("live ssh args", &args, "BatchMode=yes");
    assert_contains("live ssh args", &args, "IdentitiesOnly=yes");
    assert_contains("live ssh args", &args, "StrictHostKeyChecking=yes");
    assert_contains("live ssh args", &args, "IdentityAgent=none");
    assert_contains("live ssh args", &args, "IdentityFile=none");
    assert_contains(
        "live ssh args",
        &args,
        &format!(
            "UserKnownHostsFile={}",
            common_dir.join("wrix/github_known_hosts").display()
        ),
    );
    assert_contains("live ssh args", &args, &deploy_key.display().to_string());
    assert_contains("live ssh args", &args, "git@github.com");
    assert_contains(
        "live ssh args",
        &args,
        &format!("git-upload-pack '{expected_repo}'"),
    );
    assert_not_contains("live ssh args", &args, "StrictHostKeyChecking=no");

    let ssh_env = fs::read_to_string(ssh_capture_dir.join("env"))?;
    assert_minimal_online_env("online SSH env", &ssh_env, deploy_key, home);
    Ok(())
}

fn assert_minimal_online_env(label: &str, output: &str, deploy_key: &Path, home: &Path) {
    assert_contains(label, output, "GIT_CONFIG_GLOBAL=/dev/null");
    assert_contains(label, output, "GIT_CONFIG_NOSYSTEM=1");
    assert_contains(label, output, "GIT_TERMINAL_PROMPT=0");
    assert_contains(label, output, "GIT_SSH_VARIANT=ssh");
    assert_contains(label, output, &format!("HOME={}", home.display()));
    assert_contains(
        label,
        output,
        &format!("WRIX_DEPLOY_KEY={}", deploy_key.display()),
    );
    assert_not_contains(label, output, "GIT_SSH_COMMAND=");
    assert_not_contains(label, output, "SSH_AUTH_SOCK=");
    assert_not_contains(label, output, "WRIX_SHOULD_NOT_LEAK=");
}

fn assert_absent(path: &Path) {
    assert!(!path.exists(), "unexpected file exists: {}", path.display());
}

#[test]
fn defaults_and_overrides_do_not_create_policy() -> TestResult {
    const CHILD: &str = "WRIX_TEST_INIT_POLICY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = std::env::current_dir()?;
        let directory = tempfile::tempdir()?;
        let profile = directory.path().join("profile.json");
        fs::write(&profile, r#"{"security":{"deploy_key":"profile-key"}}"#)?;
        let derived = format!("{}-test-host", root.file_name().unwrap().to_str().unwrap());
        run_git(
            &root,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:example/upstream.git",
            ],
        )?;
        for (input, key, flags) in [
            (None, derived.as_str(), vec![]),
            (Some(profile.as_path()), "profile-key", vec![]),
            (Some(profile.as_path()), "repo-key", vec![]),
            (
                Some(profile.as_path()),
                "flag-key",
                vec![
                    "--key",
                    "flag-key",
                    "--remote",
                    "origin",
                    "--offline",
                    "--no-hooks",
                    "--no-sign",
                ],
            ),
        ] {
            if key == "repo-key" {
                fs::write(
                    root.join("wrix.toml"),
                    "[git]\ndeploy_key = 'repo-key'\nremote = 'upstream'\n[init]\nprek_hooks = false\nonline_verify = false\n",
                )?;
            }
            if key == "flag-key" {
                fs::write(
                    root.join("wrix.toml"),
                    "[git]\ndeploy_key = 'repo-key'\nremote = 'upstream'\nsign = true\n[init]\nprek_hooks = true\nonline_verify = true\n",
                )?;
            }
            let policy = root.join("wrix.toml");
            let before = if policy.exists() {
                Some(tree(&policy)?)
            } else {
                None
            };
            let mut args = vec!["repo"];
            args.extend(flags);
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let result = wrix_cli::init::run(input, &args, &mut stdout, &mut stderr)?;
            assert_eq!(
                result,
                std::process::ExitCode::SUCCESS,
                "{}",
                String::from_utf8_lossy(&stderr)
            );
            let stdout = String::from_utf8(stdout)?;
            assert_contains("key precedence", &stdout, &format!("deploy_key: {key}\n"));
            for line in ["sign: false", "prek_hooks: false"] {
                assert_contains("policy", &stdout, line);
            }
            assert_contains(
                "remote precedence",
                &stdout,
                if key == "repo-key" {
                    "remote: upstream"
                } else {
                    "remote: origin"
                },
            );
            assert_contains(
                "online precedence",
                &stdout,
                if matches!(key, "repo-key" | "flag-key") {
                    "online_verify: false"
                } else {
                    "online_verify: true"
                },
            );
            if let Some(before) = before {
                assert_eq!(tree(&policy)?, before);
            } else {
                assert!(!policy.exists());
            }
        }
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let key = fixture.home.join("deploy-key");
    write_empty_key(&key)?;
    let mut command = Command::new(std::env::current_exe()?);
    common::set_isolated_git_env(&mut command);
    let result = run_command(
        command
            .args([
                "--exact",
                "defaults_and_overrides_do_not_create_policy",
                "--nocapture",
            ])
            .current_dir(fixture.repo.path())
            .env(CHILD, "1")
            .env("HOSTNAME", "test-host")
            .env("HOME", &fixture.home)
            .env("WRIX_DEPLOY_KEY", key)
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(fixture.git.clone())
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )?,
            ),
    )?;
    assert!(
        result.status.success(),
        "{} {}",
        result.stdout,
        result.stderr
    );
    Ok(())
}

#[test]
fn required_signing_material_is_not_implicitly_provisioned() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["repo", "--offline", "--sign", "--key", "key"],
        vec!["--offline", "--sign", "--key", "key"],
    ] {
        fixture.assert_rejected_unchanged(&args, "signing")?;
    }
    Ok(())
}

#[test]
fn repo_applies_and_verifies_without_implicit_provisioning() -> TestResult {
    let fixture = Fixture::new()?;
    common::write_ed25519_key(&fixture.home.join(".ssh/deploy_keys/key"))?;
    let home = tree(&fixture.home)?;
    assert_success_with_clean_stderr(&fixture.run(&["repo", "--offline", "--key", "key"])?);
    assert!(fixture.repo.path().join(".git/wrix/git-ssh").is_file());
    assert_eq!(tree(&fixture.home)?, home);
    assert_eq!(fixture.log("gh.log")?, "");
    assert_eq!(fixture.log("keygen.log")?, "");
    assert_success_with_clean_stderr(&fixture.run(&["--offline", "--key", "key"])?);
    Ok(())
}

#[test]
fn outer_repo_setup_does_not_mutate_integration_clone() -> TestResult {
    let fixture = Fixture::new()?;
    let integration = clone_integration(fixture.repo.path())?;
    let before = tree(&integration)?;
    assert_success_with_clean_stderr(
        &fixture.run(&["repo", "--deploy", "--sign", "--key", "key"])?,
    );
    assert_eq!(tree(&integration)?, before);
    assert!(!integration.join(".git/wrix").exists());
    Ok(())
}

#[test]
fn clone_and_linked_worktree_scope_is_preserved() -> TestResult {
    let fixture = Fixture::new()?;
    let integration = clone_integration(fixture.repo.path())?;
    let outer = tree(&fixture.repo.path().join(".git"))?;
    run_git(
        &integration,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:example/integration.git",
        ],
    )?;
    let mut command = fixture.command(&["repo", "--deploy", "--sign", "--key", "key"])?;
    assert_success_with_clean_stderr(&run_command(command.current_dir(&integration))?);
    assert_eq!(tree(&fixture.repo.path().join(".git"))?, outer);
    let worktree = fixture.directory.path().join("linked");
    run_git(
        &integration,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            worktree.to_str().unwrap(),
        ],
    )?;
    assert_eq!(common_git_dir(&worktree)?, common_git_dir(&integration)?);
    let before = tree(&integration.join(".git"))?;
    let mut command = fixture.command(&["repo", "--offline", "--sign", "--key", "key"])?;
    assert_success_with_clean_stderr(&run_command(command.current_dir(&worktree))?);
    assert_eq!(tree(&integration.join(".git"))?, before);
    for key in [
        "core.sshCommand",
        "user.signingkey",
        "gpg.ssh.allowedSignersFile",
    ] {
        assert_eq!(
            git_stdout(&worktree, &["config", key])?,
            git_stdout(&integration, &["config", key])?
        );
    }
    let mut command = fixture.command(&["--offline", "--sign", "--key", "key"])?;
    assert_success_with_clean_stderr(&run_command(command.current_dir(&integration))?);
    assert_eq!(tree(&fixture.repo.path().join(".git"))?, outer);
    Ok(())
}

#[test]
fn repo_signing_is_opt_in_and_verified() -> TestResult {
    let fixture = Fixture::new()?;
    let objects = tree(&fixture.repo.path().join(".git/objects"))?;
    assert_success_with_clean_stderr(&fixture.run(&["repo", "--deploy", "--key", "key"])?);
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", "commit.gpgsign"])?,
        "false"
    );
    assert!(!fixture.home.join(".ssh/deploy_keys/key-signing").exists());
    assert_success_with_clean_stderr(
        &fixture.run(&["repo", "--deploy", "--sign", "--key", "key"])?,
    );
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", "commit.gpgsign"])?,
        "true"
    );
    assert_eq!(tree(&fixture.repo.path().join(".git/objects"))?, objects);
    let log = fixture.log("keygen.log")?;
    assert_contains("disposable signature", &log, "-Y\nsign\n");
    assert_contains("signature verification", &log, "-Y\nverify\n");
    let grants = wrix_core::repository_policy::read(fixture.repo.path())?.git;
    assert!(!grants.deploy_enabled());
    assert!(!grants.sign_enabled());
    assert_passive(&fixture, &["--offline", "--sign", "--key", "key"], true)?;
    Ok(())
}

#[test]
fn signing_modes_apply_or_verify_without_rewriting_policy() -> TestResult {
    let fixture = prepared_signed_fixture()?;
    for policy in [true, false] {
        let path = fixture.repo.path().join("wrix.toml");
        fs::write(&path, format!("[git]\nsign = {policy}\n"))?;
        let before = tree(&path)?;
        for (flag, expected) in [("--no-sign", "false"), ("--sign", "true")] {
            assert_success_with_clean_stderr(&fixture.run(&[
                "repo",
                "--offline",
                flag,
                "--key",
                "key",
            ])?);
            assert_eq!(
                git_stdout(fixture.repo.path(), &["config", "commit.gpgsign"])?,
                expected
            );
            assert_passive(&fixture, &["--offline", flag, "--key", "key"], true)?;
            let opposite = if flag == "--sign" {
                "--no-sign"
            } else {
                "--sign"
            };
            assert_passive(&fixture, &["--offline", opposite, "--key", "key"], false)?;
            assert_eq!(tree(&path)?, before);
        }
    }
    Ok(())
}

#[test]
fn repo_hooks_are_optional_and_clone_scoped() -> TestResult {
    let fixture = prepared_signed_fixture()?;
    let integration = clone_integration(fixture.repo.path())?;
    let clone_before = tree(&integration)?;
    let runtime = common::prek::runtime()?;
    let config = fixture.repo.path().join(".pre-commit-config.yaml");
    let apply = |flags: &[&str]| -> TestResult {
        let mut args = vec!["repo", "--offline", "--sign", "--key", "key"];
        args.extend_from_slice(flags);
        let mut command = fixture.command(&args)?;
        command
            .env("WRIX_PREK_RUNNER", &runtime.runner)
            .env("WRIX_PREK_HOOKS", &runtime.hooks);
        assert_success_with_clean_stderr(&run_command(&mut command)?);
        Ok(())
    };
    apply(&[])?;
    assert!(!git_stdout(fixture.repo.path(), &["config", "--list"])?.contains("core.hookspath="));
    fs::write(&config, "repos: []\n")?;
    apply(&[])?;
    assert_eq!(
        git_stdout(fixture.repo.path(), &["config", "core.hooksPath"])?,
        runtime.hooks.display().to_string()
    );
    let installed = tree(&fixture.repo.path().join(".git"))?;
    apply(&[])?;
    assert_eq!(tree(&fixture.repo.path().join(".git"))?, installed);
    for policy in [false, true] {
        fs::write(
            fixture.repo.path().join("wrix.toml"),
            format!("[init]\nprek_hooks = {policy}\n"),
        )?;
        run_git(
            fixture.repo.path(),
            &["config", "core.hooksPath", "/custom/hooks"],
        )?;
        let before = tree(&fixture.repo.path().join(".git"))?;
        apply(if policy { &["--no-hooks"] } else { &[] })?;
        assert_eq!(tree(&fixture.repo.path().join(".git"))?, before);
    }
    assert_eq!(tree(&integration)?, clone_before);
    Ok(())
}

#[test]
fn composed_repo_initialization_is_idempotent() -> TestResult {
    let fixture = prepared_signed_fixture()?;
    let repo = tree(fixture.repo.path())?;
    let home = tree(&fixture.home)?;
    let remote = tree(&fixture.directory.path().join("state"))?;
    let args = ["repo", "--deploy", "--sign", "--key", "key"];
    fs::write(fixture.directory.path().join("gh.log"), "")?;
    fs::write(fixture.directory.path().join("keygen.log"), "")?;
    assert_success_with_clean_stderr(&fixture.run(&args)?);
    assert_eq!(tree(fixture.repo.path())?, repo);
    assert_eq!(tree(&fixture.home)?, home);
    assert_eq!(tree(&fixture.directory.path().join("state"))?, remote);
    assert_not_contains("reuse", &fixture.log("gh.log")?, "POST");
    assert_not_contains("reuse", &fixture.log("gh.log")?, "DELETE");
    assert_not_contains("reuse", &fixture.log("keygen.log")?, "-t\ned25519");
    assert!(!fixture.repo.path().join("wrix.toml").exists());
    Ok(())
}

fn prepared_signed_fixture() -> TestResult<Fixture> {
    let fixture = Fixture::new()?;
    let result = fixture.run(&["repo", "--deploy", "--sign", "--key", "key"])?;
    assert_success_with_clean_stderr(&result);
    Ok(fixture)
}

fn assert_passive(fixture: &Fixture, args: &[&str], succeeds: bool) -> TestResult<RunResult> {
    let repo = tree(fixture.repo.path())?;
    let home = tree(&fixture.home)?;
    let remote = tree(&fixture.directory.path().join("state"))?;
    let gh_log = fixture.log("gh.log")?;
    let result = fixture.run(args)?;
    assert_eq!(result.status.success(), succeeds, "{}", result.stderr);
    assert_eq!(tree(fixture.repo.path())?, repo, "repository changed");
    assert_eq!(tree(&fixture.home)?, home, "credential state changed");
    assert_eq!(tree(&fixture.directory.path().join("state"))?, remote);
    assert_eq!(
        fixture.log("gh.log")?,
        gh_log,
        "verification used GitHub API"
    );
    Ok(result)
}

#[test]
fn bare_verification_never_applies_or_repairs_state() -> TestResult {
    let fixture = prepared_signed_fixture()?;
    let args = ["--offline", "--sign", "--key", "key"];
    assert_passive(&fixture, &args, true)?;
    fs::write(
        fixture.repo.path().join(".git/wrix/allowed_signers"),
        "invalid\n",
    )?;
    assert_passive(&fixture, &args, false)?;
    Ok(())
}

#[test]
fn verification_reports_drift_without_repair() -> TestResult {
    for artifact in ["git-ssh", "github_known_hosts"] {
        let fixture = prepared_signed_fixture()?;
        let path = fixture.repo.path().join(".git/wrix").join(artifact);
        fs::write(
            &path,
            "#!/usr/bin/env bash\nset -euo pipefail\ntouch lazy-repair\n",
        )?;
        let result = assert_passive(&fixture, &["--offline", "--sign", "--key", "key"], false)?;
        assert_contains("repair guidance", &result.stderr, "wrix init repo");
        assert!(!fixture.repo.path().join("lazy-repair").exists());
        fs::remove_file(&path)?;
        assert_passive(&fixture, &["--offline", "--sign", "--key", "key"], false)?;
    }
    let fixture = Fixture::new()?;
    assert_passive(&fixture, &["--offline", "--key", "key"], false)?;
    Ok(())
}

#[test]
fn offline_signing_checks_reject_effective_overrides_without_repair() -> TestResult {
    let fixture = prepared_signed_fixture()?;
    run_git(
        fixture.repo.path(),
        &["config", "extensions.worktreeConfig", "true"],
    )?;
    run_git(
        fixture.repo.path(),
        &["config", "--worktree", "gpg.ssh.program", "false"],
    )?;
    assert_passive(&fixture, &["--offline", "--sign", "--key", "key"], false)?;
    Ok(())
}

#[test]
fn offline_hook_checks_reject_drift_without_binding_or_repair() -> TestResult {
    assert_offline_hooks(false)?;
    assert_offline_hooks(true)
}

fn assert_offline_hooks(policy: bool) -> TestResult {
    let fixture = Fixture::new()?;
    let runtime = common::prek::runtime()?;
    fs::write(
        fixture.repo.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )?;
    let mut command = fixture.command(&["repo", "--deploy", "--key", "key"])?;
    command
        .env("WRIX_PREK_HOOKS", &runtime.hooks)
        .env("WRIX_PREK_RUNNER", &runtime.runner);
    assert_success_with_clean_stderr(&run_command(&mut command)?);
    let mut args = vec!["--key", "key"];
    if policy {
        fs::write(
            fixture.repo.path().join("wrix.toml"),
            "[init]\nonline_verify = false\n",
        )?;
    } else {
        args.push("--offline");
    }
    let binding = git_stdout(
        fixture.repo.path(),
        &["config", "--name-only", "--get-regexp", "^wrix[.]prek-"],
    )?;
    for artifact in ["configured", "core.hooksPath", binding.as_str()] {
        if artifact != "configured" {
            run_git(
                fixture.repo.path(),
                &["config", artifact, "/missing-runtime"],
            )?;
        }
        let before = tree(fixture.repo.path())?;
        let home = tree(&fixture.home)?;
        let gh_log = fixture.log("gh.log")?;
        let mut command = fixture.command(&args)?;
        command
            .env("WRIX_PREK_HOOKS", &runtime.hooks)
            .env("WRIX_PREK_RUNNER", &runtime.broken_runner);
        let result = run_command(&mut command)?;
        assert_eq!(
            result.status.success(),
            artifact == "configured",
            "{}",
            result.stderr
        );
        assert_eq!(tree(fixture.repo.path())?, before);
        assert_eq!(tree(&fixture.home)?, home);
        assert_eq!(fixture.log("gh.log")?, gh_log);
        run_git(
            fixture.repo.path(),
            &[
                "config",
                "core.hooksPath",
                &runtime.hooks.display().to_string(),
            ],
        )?;
    }
    Ok(())
}

fn assert_offline_local_checks(policy: bool) -> TestResult {
    let fixture = prepared_signed_fixture()?;
    let mut args = vec!["--sign", "--key", "key"];
    if policy {
        fs::write(
            fixture.repo.path().join("wrix.toml"),
            "[init]\nonline_verify = false\n",
        )?;
    } else {
        args.push("--offline");
    }
    assert_passive(&fixture, &args, true)?;
    let key = fixture.home.join(".ssh/deploy_keys/key");
    let contents = fs::read(&key)?;
    fs::remove_file(&key)?;
    assert_passive(&fixture, &args, false)?;
    fs::write(&key, contents)?;
    set_mode(&key, 0o600)?;
    let helper = fixture.repo.path().join(".git/wrix/git-ssh");
    let contents = fs::read(&helper)?;
    fs::write(&helper, "stale helper")?;
    assert_passive(&fixture, &args, false)?;
    fs::write(&helper, contents)?;
    fs::write(
        fixture.repo.path().join(".git/wrix/allowed_signers"),
        "invalid\n",
    )?;
    assert_passive(&fixture, &args, false)?;
    assert_offline_hooks(policy)?;
    Ok(())
}
