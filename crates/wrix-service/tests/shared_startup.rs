#![cfg(target_os = "linux")]

use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    thread,
    time::Duration,
};
use wrix_service::dolt::{Credentials, Sync, broker};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn set_mode(path: &Path, mode: u32) -> TestResult {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}
#[path = "../../wrix-cli/tests/common/celler.rs"]
mod celler;
#[path = "../../wrix-cli/tests/common/service_binding.rs"]
#[expect(
    dead_code,
    reason = "the shared external image-binding fixture also provides legacy issuer setup not used by fresh shared startup"
)]
mod service_binding;

#[test]
fn subprocess_entry() -> TestResult {
    let Some(arguments) = std::env::var_os("WRIX_SHARED_ARGUMENTS") else {
        return Ok(());
    };
    let arguments: Vec<String> = serde_json::from_str(arguments.to_str().unwrap())?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = wrix_cli::command::run(&arguments, &mut stdout, &mut stderr);
    let success = match result {
        Ok(status) => status == ExitCode::SUCCESS,
        Err(error) => {
            writeln!(stderr, "{error}")?;
            false
        }
    };
    fs::write(
        std::env::var_os("WRIX_SHARED_RESULT").unwrap(),
        serde_json::to_vec(
            &json!({"success":success,"stdout":String::from_utf8(stdout)?,"stderr":String::from_utf8(stderr)?}),
        )?,
    )?;
    Ok(())
}

struct Fixture {
    root: tempfile::TempDir,
    _port: fs::File,
    _celler: celler::Server,
    first: PathBuf,
    second: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::warn!(%error, "native fixture cleanup failed");
        }
    }
}

impl Fixture {
    fn cleanup(&self) -> TestResult {
        let path = self.root.path().join("runtime.json");
        if path.exists() {
            let state: Value = serde_json::from_slice(&fs::read(path)?)?;
            let pid = state["pid"].as_u64().ok_or("fixture process ID missing")?;
            let output = Command::new("kill")
                .args(["-TERM", "--", &format!("-{pid}")])
                .output()?;
            if !output.status.success() {
                tracing::warn!("fixture process already exited");
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    fn new() -> TestResult<Self> {
        let port = fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open("/tmp/wrix-shared-native-port.lock")?;
        port.lock_exclusive()?;
        let reservation = TcpListener::bind(("127.0.0.1", 3306))?;
        drop(reservation);
        let root = tempfile::Builder::new().prefix("ws-").tempdir_in("/tmp")?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        fs::create_dir(root.path().join("home"))?;
        fs::create_dir(root.path().join("host-context"))?;
        for name in ["podman", "container"] {
            fs::write(bin.join(name), include_str!("fixtures/shared-runtime.py"))?;
            set_mode(&bin.join(name), 0o755)?;
        }
        fs::write(
            bin.join("flock"),
            "#!/usr/bin/env bash\nset -euo pipefail\nexec sleep 100000\n",
        )?;
        set_mode(&bin.join("flock"), 0o755)?;
        let native_bd = std::env::var("WRIX_REAL_BD_BIN").unwrap_or_else(|_| {
            "/nix/store/7aalmix7lxvs73apxl1pwkrvqw52snhm-beads-1.3.1/bin/bd".into()
        });
        assert!(
            Path::new(&native_bd).is_file(),
            "native Beads executable unavailable"
        );
        fs::write(
            bin.join("bd"),
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\n{} \"$@\" 2>\"$WRIX_SHARED_FIXTURE/native-bd.err\" | tee \"$WRIX_SHARED_FIXTURE/native-bd.out\"\n",
                native_bd.trim()
            ),
        )?;
        set_mode(&bin.join("bd"), 0o755)?;
        let native_package = root.path().join("native-package");
        fs::create_dir_all(native_package.join("bin"))?;
        fs::rename(bin.join("bd"), native_package.join("bin/bd"))?;
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/beads/package.nix");
        let expression = format!(
            "import {} {{ pkgs = {{ writeShellScriptBin = name: script: script; beads = {}; }}; }}",
            package.display(),
            serde_json::to_string(&native_package)?
        );
        let rendered = Command::new("nix")
            .args([
                "eval",
                "--raw",
                "--impure",
                "--store",
                "dummy://",
                "--expr",
                &expression,
            ])
            .output()?;
        assert!(
            rendered.status.success(),
            "{}",
            String::from_utf8_lossy(&rendered.stderr)
        );
        fs::write(
            bin.join("bd"),
            [b"#!/usr/bin/env bash\n".as_slice(), &rendered.stdout].concat(),
        )?;
        set_mode(&bin.join("bd"), 0o755)?;
        fs::write(bin.join("wrix"), include_str!("fixtures/shared-cli.py"))?;
        set_mode(&bin.join("wrix"), 0o755)?;
        service_binding::create(root.path(), &bin, "nix-descriptor")?;
        let first = root.path().join("first");
        let second = root.path().join("second");
        for workspace in [&first, &second] {
            fs::create_dir(workspace)?;
            required(Command::new("git").args(["init", "-q"]).arg(workspace))?;
            fs::write(workspace.join("tracked"), "retained branch content")?;
            required(
                Command::new("git")
                    .args(["add", "tracked"])
                    .current_dir(workspace),
            )?;
            required(
                Command::new("git")
                    .args([
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                        "commit",
                        "-qm",
                        "initial",
                    ])
                    .current_dir(workspace),
            )?;
            let worktree = workspace.join(".git/beads-worktrees/beads");
            required(
                Command::new("git")
                    .args(["worktree", "add", "-qb", "beads"])
                    .arg(&worktree)
                    .current_dir(workspace),
            )?;
            let upstream = root.path().join(format!(
                "{}-upstream.git",
                workspace
                    .file_name()
                    .ok_or("workspace name missing")?
                    .to_string_lossy()
            ));
            required(
                Command::new("git")
                    .args(["init", "--bare", "-q"])
                    .arg(&upstream),
            )?;
            required(
                Command::new("git")
                    .args(["remote", "add", "origin"])
                    .arg(&upstream)
                    .current_dir(workspace),
            )?;
            required(
                Command::new("git")
                    .args(["push", "-u", "origin", "beads"])
                    .current_dir(&worktree),
            )?;
            fs::write(worktree.join("tracked"), "staged beads state")?;
            required(
                Command::new("git")
                    .args(["add", "tracked"])
                    .current_dir(&worktree),
            )?;
            fs::write(worktree.join("tracked"), "unstaged beads state")?;
            fs::create_dir_all(workspace.join(".beads/dolt/beads"))?;
            fs::write(workspace.join(".beads/config.yaml"), "sync-branch: beads\n")?;
            fs::write(
                workspace.join(".beads/metadata.json"),
                "{\"database\":\"dolt\",\"backend\":\"dolt\",\"dolt_database\":\"beads\",\"dolt_mode\":\"server\"}",
            )?;
            let source = workspace.join(".beads/dolt/beads");
            let backup_url = format!(
                "file://{}",
                root.path().join("same-history-backup").display()
            );
            if workspace == &second {
                native(
                    source.parent().ok_or("source parent missing")?,
                    &["config", "--global", "--add", "user.name", "Fixture"],
                )?;
                native(
                    source.parent().ok_or("source parent missing")?,
                    &[
                        "config",
                        "--global",
                        "--add",
                        "user.email",
                        "fixture@example.invalid",
                    ],
                )?;
                native(
                    source.parent().ok_or("source parent missing")?,
                    &["backup", "restore", "--force", &backup_url, "beads"],
                )?;
                continue;
            }
            // Seed a real current Beads schema before adding dirty native
            // roots. A marker-only database would invoke schema migration during
            // readiness instead of exercising an unchanged existing workspace.
            let seed = root.path().join("beads-seed");
            fs::create_dir(&seed)?;
            required(Command::new("git").args(["init", "-q"]).arg(&seed))?;
            let mut initialize = Command::new(&native_bd);
            initialize
                .args([
                    "init",
                    "--skip-hooks",
                    "--skip-agents",
                    "--non-interactive",
                    "--prefix",
                    "fixture",
                ])
                .current_dir(&seed)
                .env("HOME", root.path().join("home"))
                .env("BEADS_DOLT_MODE", "embedded");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("BEADS_") {
                    initialize.env_remove(key);
                }
            }
            initialize.env("BEADS_DOLT_MODE", "embedded");
            required(&mut initialize)?;
            fs::remove_dir(&source)?;
            fs::rename(seed.join(".beads/embeddeddolt/fixture"), &source)?;
            native(
                &source,
                &[
                    "sql",
                    "-q",
                    "CREATE TABLE marker(id INT PRIMARY KEY); INSERT INTO marker VALUES(1); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','checkpoint'); CALL DOLT_BRANCH('retained'); CALL DOLT_TAG('retainedtag'); INSERT INTO marker VALUES(2); CALL DOLT_ADD('.'); INSERT INTO marker VALUES(3)",
                ],
            )?;
            native(&source, &["backup", "sync-url", &backup_url])?;
        }
        let celler = celler::Server::watch(root.path(), None);
        Ok(Self {
            root,
            _port: port,
            _celler: celler,
            first,
            second,
        })
    }

    fn command(&self, workspace: &Path, arguments: &[&str]) -> TestResult<Command> {
        // Only this disposable namespace hides the image-agent marker. The
        // outer worker's host capability boundary is never modified/bypassed.
        let mut command = Command::new("timeout");
        command
            .args([
                "--signal=KILL",
                "180",
                "unshare",
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "private",
                "sh",
                "-c",
                "mount --bind \"$1\" /etc/wrix; shift; exec \"$@\"",
                "fixture",
            ])
            .arg(self.root.path().join("host-context"))
            .arg(std::env::current_exe()?);
        let path = std::env::join_paths(std::iter::once(self.root.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("PATH missing")?),
        ))?;
        command
            .args(["--exact", "subprocess_entry", "--nocapture"])
            .current_dir(workspace)
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env(
                "WRIX_SERVICE_PACKAGE_BINDING",
                self.root.path().join("binding.json"),
            )
            .env("WRIX_SHARED_ARGUMENTS", serde_json::to_string(arguments)?)
            .env("WRIX_SHARED_RESULT", self.root.path().join("result.json"))
            .env("WRIX_SHARED_FIXTURE", self.root.path())
            .env("WRIX_SHARED_TEST_EXE", std::env::current_exe()?)
            .env("WRIX_BIN", self.root.path().join("bin/wrix"))
            .env(
                "WRIX_SHARED_BINARY",
                env!("CARGO_BIN_EXE_wrix-shared-service"),
            )
            .env(
                "WRIX_CONTAINER_RUNTIME",
                self.root.path().join("bin/podman"),
            )
            .env("WRIX_DOLT_TRANSPORT", "unix")
            .env_remove("WRIX_SERVICE_IMAGE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST")
            .env_remove("LOOM_INSIDE")
            .env_remove("IS_SANDBOX");
        Ok(command)
    }

    fn run(&self, workspace: &Path, arguments: &[&str]) -> TestResult<Value> {
        let output = self.command(workspace, arguments)?.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&fs::read(
            self.root.path().join("result.json"),
        )?)?)
    }

    fn start(&self, workspace: &Path) -> TestResult<Value> {
        let output = self.run(workspace, &["service", "start", "--no-cache"])?;
        if output["success"] != true {
            let log = self.root.path().join("service.log");
            return Err(format!(
                "start failed: {output}; native service: {}",
                if log.exists() {
                    fs::read_to_string(log)?
                } else {
                    "not created".into()
                }
            )
            .into());
        }
        assert!(
            output["stdout"]
                .as_str()
                .ok_or("start status missing")?
                .contains("Running"),
            "reactivation must report active SQL, not an old cache retirement: {output}"
        );
        let ready = self.run(workspace, &["service", "dolt", "wait"])?;
        assert_eq!(
            ready["success"],
            true,
            "authenticated native Beads readiness failed: {ready}; {} {}",
            fs::read_to_string(self.root.path().join("native-bd.out"))?,
            fs::read_to_string(self.root.path().join("native-bd.err"))?
        );
        let output = self.run(workspace, &["service", "dolt", "client"])?;
        assert_eq!(
            output["success"],
            true,
            "{output}; runtime {}",
            fs::read_to_string(self.root.path().join("runtime.log"))?
        );
        Ok(serde_json::from_str(
            output["stdout"].as_str().ok_or("client output missing")?,
        )?)
    }
}

fn required(command: &mut Command) -> TestResult {
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn native(source: &Path, args: &[&str]) -> TestResult {
    required(
        Command::new("dolt")
            .args(args)
            .current_dir(source)
            .env(
                "HOME",
                source.parent().ok_or("native source parent missing")?,
            )
            .env_remove("DOLT_ROOT_HOST")
            .env_remove("DOLT_ROOT_PASSWORD")
            .env_remove("DOLT_CLI_PASSWORD"),
    )
}

fn sql(client: &Value, query: &str) -> TestResult<std::process::Output> {
    Ok(Command::new("dolt")
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            "3306",
            "--no-tls",
            "--password",
            client["credentials"]["password"]
                .as_str()
                .ok_or("SQL fixture credential missing")?,
            "--user",
            client["credentials"]["user"]
                .as_str()
                .ok_or("SQL fixture credential missing")?,
            "--use-db",
            client["credentials"]["database"]
                .as_str()
                .ok_or("SQL fixture credential missing")?,
            "sql",
            "-r",
            "json",
            "-q",
            query,
        ])
        .env(
            "DOLT_CLI_PASSWORD",
            client["credentials"]["password"]
                .as_str()
                .ok_or("SQL fixture credential missing")?,
        )
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .output()?)
}

#[test]
fn two_namespaces_start_register_sync_and_retire_on_one_native_shared_backend() -> TestResult {
    if std::env::var_os("WRIX_SHARED_ISOLATED_NETWORK").is_none() {
        // The production host port allocator must see a real private network,
        // not race unrelated parallel fixture Celler listeners on this worker.
        required(
            Command::new("timeout")
                .args([
                    "--signal=KILL",
                    "180",
                    "unshare",
                    "--user",
                    "--map-root-user",
                    "--mount",
                    "--net",
                    "--propagation",
                    "private",
                    "sh",
                    "-c",
                    "ip link set lo up; exec \"$@\"",
                    "fixture",
                ])
                .arg(std::env::current_exe()?)
                .args([
                    "--exact",
                    "two_namespaces_start_register_sync_and_retire_on_one_native_shared_backend",
                    "--nocapture",
                ])
                .env("WRIX_SHARED_ISOLATED_NETWORK", "1"),
        )?;
        return Ok(());
    }
    let fixture = Fixture::new()?;
    let acknowledged = fixture.run(
        &fixture.first,
        &["workspace", "acknowledge-scope", "--version", "1"],
    )?;
    assert_eq!(acknowledged["success"], true, "{acknowledged}");
    let legacy_path = fixture.root.path().join("retained-legacy.json");
    let legacy = json!([{"configuration":{"id":"retained-legacy", "labels":{"wrix.kind":"service"}, "publishedPorts":[]}, "status":{"state":"stopped"}}]);
    let retained_legacy = serde_json::to_vec(&legacy)?;
    fs::write(&legacy_path, &retained_legacy)?;
    let recovery = fixture.run(&fixture.first, &["service", "recover"])?;
    assert_eq!(recovery["success"], true, "{recovery}");
    let first = fixture.start(&fixture.first)?;
    let second = fixture.start(&fixture.second)?;
    assert_eq!(
        fs::read(&legacy_path)?,
        retained_legacy,
        "maintenance must retain stopped legacy evidence, without an ordinary-start recovery loop"
    );
    let mut active_legacy = legacy;
    active_legacy[0]["status"]["state"] = "running".into();
    fs::write(&legacy_path, serde_json::to_vec(&active_legacy)?)?;
    let refused = fixture.run(&fixture.first, &["service", "start", "--no-cache"])?;
    assert_eq!(
        refused["success"], false,
        "a live legacy service must never be adopted/replaced"
    );
    fs::write(&legacy_path, &retained_legacy)?;
    for user in ["root", "wrixadmin"] {
        for password in ["", "wrong-private-password"] {
            let mut unauthorized = first.clone();
            unauthorized["credentials"]["user"] = user.into();
            unauthorized["credentials"]["password"] = password.into();
            assert!(
                !sql(&unauthorized, "SELECT CURRENT_USER()")?
                    .status
                    .success(),
                "blank/wrong administrator credential admitted"
            );
        }
    }
    assert_ne!(
        first["credentials"]["database"],
        second["credentials"]["database"]
    );
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    let calls: Vec<Vec<String>> = calls
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let runs: Vec<_> = calls.iter().filter(|call| call[0] == "run").collect();
    assert_eq!(runs.len(), 1);
    assert!(
        !runs[0]
            .iter()
            .any(|argument| argument.contains("host-control")
                || argument.contains("/control/authority")
                || argument.contains("/first/.git")
                || argument.contains("/second/.git")),
        "backend received host Git/controller authority"
    );
    assert!(
        runs[0]
            .iter()
            .any(|argument| argument == "wrix.service.scope=host")
    );
    assert!(!runs[0].iter().any(|argument| argument.ends_with(":3306")
        || argument.ends_with(":3307")
        || argument.contains("host-control")
        || argument.contains("/first")
        || argument.contains("/second")));
    assert!(!calls.iter().any(|call| call[0] == "rm"));
    let rows = sql(&first, "SELECT * FROM marker ORDER BY id")?;
    assert!(
        rows.status.success(),
        "{}",
        String::from_utf8_lossy(&rows.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&rows.stdout)?["rows"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let status = sql(&first, "SELECT staged FROM dolt_status")?;
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout)?["rows"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(fixture.first.join(".beads/dolt/WRIX-MANAGED").is_file());
    assert!(!fixture.first.join(".beads/dolt/beads/.dolt").exists());
    assert!(
        Path::new(first["git_common"].as_str().unwrap()).starts_with(
            fixture
                .root
                .path()
                .join("state/wrix/host-control/beads-git")
        )
    );
    let source = first["worktree"].as_str().unwrap();
    assert!(
        Path::new(source).starts_with(
            fixture
                .root
                .path()
                .join("state/wrix/service-data/dolt/namespaces")
        )
    );
    assert_eq!(
        fs::read_to_string(Path::new(source).join("tracked"))?,
        "unstaged beads state"
    );
    let staged = Command::new("git")
        .arg("--git-dir")
        .arg(first["git_common"].as_str().unwrap())
        .args(["show", ":tracked"])
        .current_dir(source)
        .output()?;
    assert!(staged.status.success());
    assert_eq!(String::from_utf8(staged.stdout)?, "staged beads state");
    assert_ne!(
        Path::new(first["git_common"].as_str().unwrap()),
        fixture.first.join(".git")
    );
    let denied = sql(
        &first,
        &format!(
            "SELECT * FROM `{}`.marker",
            second["credentials"]["database"].as_str().unwrap()
        ),
    )?;
    assert!(!denied.status.success());
    assert!(
        !sql(&first, "CALL DOLT_REMOTE('add','evil','file:///tmp/evil')")?
            .status
            .success()
    );
    assert!(sql(&first, "INSERT INTO marker VALUES(4); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','scoped write')")?.status.success());
    let synced = fixture.run(&fixture.first, &["service", "dolt", "sync-push"])?;
    assert_eq!(synced["success"], true, "{synced}");
    let pulled = fixture.run(&fixture.first, &["service", "dolt", "sync-pull"])?;
    assert_eq!(pulled["success"], true, "{pulled}");
    // Poison the guest-only metadata and the shared worktree pointer. Host
    // delivery and Git sync must never open this metadata or execute its hooks.
    let sandbox_client = fixture.run(&fixture.first, &["service", "dolt", "sandbox-client"])?;
    assert_eq!(sandbox_client["success"], true);
    let sandbox_client: Value = serde_json::from_str(sandbox_client["stdout"].as_str().unwrap())?;
    let guest_git = PathBuf::from(sandbox_client["git_common"].as_str().unwrap());
    assert!(
        guest_git.starts_with(
            fixture
                .root
                .path()
                .join("state/wrix/service-data/dolt/namespaces")
        )
    );
    assert!(
        !Path::new(first["git_common"].as_str().unwrap())
            .starts_with(fixture.root.path().join("state/wrix/service-data/dolt"))
    );
    let sentinel = fixture.root.path().join("HOST-ESCAPE");
    let hooks = guest_git.join("evil-hooks");
    fs::create_dir(&hooks)?;
    fs::write(
        hooks.join("pre-commit"),
        format!(
            "#!/usr/bin/env bash\ntouch '{}'\nexit 91\n",
            sentinel.display()
        ),
    )?;
    set_mode(&hooks.join("pre-commit"), 0o755)?;
    for (key, value) in [
        ("core.hooksPath", hooks.display().to_string()),
        ("core.fsmonitor", format!("touch '{}'", sentinel.display())),
        (
            "core.sshCommand",
            format!("touch '{}'; exit 91", sentinel.display()),
        ),
    ] {
        required(
            Command::new("git")
                .args(["config", "--file"])
                .arg(guest_git.join("config"))
                .arg(key)
                .arg(value),
        )?;
    }
    fs::write(
        Path::new(source).join(".git"),
        format!("gitdir: {}\n", guest_git.display()),
    )?;
    assert_eq!(
        fixture.run(&fixture.first, &["service", "dolt", "client"])?["success"],
        true
    );
    // Exercise the complete production Beads+Git workflow with the actual
    // Nix-rendered wrapper, not only the raw SQL broker adapter.
    let full_sync = fixture.run(&fixture.first, &["beads", "push"])?;
    assert_eq!(full_sync["success"], true, "{full_sync}");
    assert!(
        !sentinel.exists(),
        "worker Git metadata executed on the host"
    );
    let endpoint: broker::Endpoint = serde_json::from_value(first["sync"].clone())?;
    let credentials: Credentials = serde_json::from_value(first["credentials"].clone())?;
    let mut forged = second["credentials"].clone();
    forged["password"] = first["credentials"]["password"].clone();
    assert_eq!(
        broker::request(&endpoint, &serde_json::from_value(forged)?, Sync::Push)?,
        broker::Reply::Rejected
    );
    if let broker::Endpoint::Unix { path } = &endpoint {
        use std::io::Read;
        for frame in [
            b"{\"namespace\":\"../peer\",\"operation\":\"push\",\"password\":\"x\"}".as_slice(),
            b"{\"operation\":\"sql\",\"query\":\"DROP DATABASE peer\"}",
            b"{broken",
        ] {
            let mut stream = std::os::unix::net::UnixStream::connect(path)?;
            stream.set_read_timeout(Some(Duration::from_secs(6)))?;
            stream.write_all(&u32::try_from(frame.len())?.to_be_bytes())?;
            stream.write_all(frame)?;
            let mut byte = [0];
            assert!(matches!(stream.read(&mut byte), Ok(0) | Err(_)));
        }
    }
    assert_eq!(
        broker::request(&endpoint, &credentials, Sync::Push)?,
        broker::Reply::Applied
    );
    assert!(sql(&second, "INSERT INTO marker VALUES(55); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','peer private')")?.status.success());
    assert_eq!(
        fixture.run(&fixture.second, &["service", "dolt", "sync-push"])?["success"],
        true
    );
    let projection = Path::new(source).join(".beads/dolt-remote");
    let saved_projection = Path::new(source).join(".beads/retained-remote");
    let peer_private = Path::new(second["worktree"].as_str().unwrap())
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("native-remote");
    assert!(peer_private.join("manifest").is_file());
    let peer_manifest = fs::read(peer_private.join("manifest"))?;
    fs::rename(&projection, &saved_projection)?;
    std::os::unix::fs::symlink(&peer_private, &projection)?;
    assert!(!matches!(
        broker::request(&endpoint, &credentials, Sync::Pull),
        Ok(broker::Reply::Applied)
    ));
    assert!(!matches!(
        broker::request(&endpoint, &credentials, Sync::Push),
        Ok(broker::Reply::Applied)
    ));
    assert_eq!(fs::read(peer_private.join("manifest"))?, peer_manifest);
    fs::remove_file(&projection)?;
    fs::rename(&saved_projection, &projection)?;
    // Swap the writable projection while root handles pulls. Opening the
    // projection is fd-relative/no-follow; a checked pathname is never handed
    // to privileged native Dolt, including after a concurrent rename.
    let racing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let worker_racing = racing.clone();
    let racing_projection = projection;
    let racing_saved = saved_projection;
    let racing_peer = peer_private.clone();
    let racer = thread::spawn(move || -> std::io::Result<()> {
        while worker_racing.load(std::sync::atomic::Ordering::Relaxed) {
            fs::rename(&racing_projection, &racing_saved)?;
            std::os::unix::fs::symlink(&racing_peer, &racing_projection)?;
            thread::yield_now();
            fs::remove_file(&racing_projection)?;
            fs::rename(&racing_saved, &racing_projection)?;
        }
        Ok(())
    });
    for _ in 0..12 {
        // A transient missing/changed own snapshot may reject; no peer data
        // may ever become readable through the authenticated caller namespace.
        let result = broker::request(&endpoint, &credentials, Sync::Pull);
        assert!(!matches!(result, Ok(broker::Reply::NonFastForward)));
    }
    racing.store(false, std::sync::atomic::Ordering::Relaxed);
    racer.join().unwrap()?;
    assert_eq!(fs::read(peer_private.join("manifest"))?, peer_manifest);
    let peer_leak = sql(&first, "SELECT COUNT(*) AS n FROM marker WHERE id=55")?;
    assert!(peer_leak.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&peer_leak.stdout)?["rows"][0]["n"],
        "0"
    );
    let retired = fixture.run(&fixture.first, &["service", "stop", "--no-cache"])?;
    assert_eq!(retired["success"], true, "{retired}");
    assert!(!sql(&first, "SELECT 1")?.status.success());
    assert_eq!(
        broker::request(&endpoint, &credentials, Sync::Push)?,
        broker::Reply::Rejected
    );
    assert!(
        sql(&second, "INSERT INTO marker VALUES(99)")?
            .status
            .success()
    );
    let reentered = fixture.start(&fixture.first)?;
    assert_eq!(reentered["credentials"], first["credentials"]);
    let runtime_path = fixture.root.path().join("runtime.json");
    let mut runtime_state: Value = serde_json::from_slice(&fs::read(&runtime_path)?)?;
    let original_pid = runtime_state["pid"].clone();
    let shared_label = runtime_state["labels"]["wrix.dolt.shared"].take();
    fs::write(&runtime_path, serde_json::to_vec(&runtime_state)?)?;
    let incompatible = fixture.run(&fixture.first, &["service", "recover"])?;
    assert_eq!(
        incompatible["success"], false,
        "live incompatible owner must not be replaced"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&runtime_path)?)?["pid"],
        original_pid
    );
    runtime_state["labels"]["wrix.dolt.shared"] = shared_label;
    fs::write(&runtime_path, serde_json::to_vec(&runtime_state)?)?;
    required(
        Command::new(fixture.root.path().join("bin/podman"))
            .args(["stop", runtime_state["name"].as_str().unwrap()])
            .env("WRIX_SHARED_FIXTURE", fixture.root.path()),
    )?;
    thread::sleep(Duration::from_millis(300));
    let recovered = fixture.run(&fixture.first, &["service", "recover"])?;
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        fixture.start(&fixture.first)?["credentials"],
        first["credentials"]
    );
    assert!(
        sql(&second, "SELECT * FROM marker WHERE id=99")?
            .status
            .success()
    );
    assert!(!sentinel.exists());
    assert!(
        sql(&reentered, "SELECT * FROM marker WHERE id=4")?
            .status
            .success()
    );
    Ok(())
}
