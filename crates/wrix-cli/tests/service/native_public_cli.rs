//! Public CLI/native SQL acceptance. Only the container boundary is mocked.
#![cfg(test)]
use crate::common::TestResult;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
    time::{Duration, Instant},
};
use tokio::process::Command;

struct RuntimeCleanup(PathBuf);
impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        let record = self.0.join("runtime.json");
        if !record.exists() {
            return;
        }
        let state: Value =
            serde_json::from_slice(&fs::read(record).expect("read private runtime record"))
                .expect("decode private runtime record");
        let group = Pid::from_raw(
            i32::try_from(state["pid"].as_u64().expect("runtime PID"))
                .expect("bounded runtime PID"),
        )
        .expect("nonzero runtime PID");
        match kill_process_group(group, Signal::TERM) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => return,
            Err(error) => panic!("terminate private runtime: {error}"),
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match test_kill_process_group(group) {
                Err(rustix::io::Errno::SRCH) => return,
                Ok(()) => {}
                Err(error) => panic!("observe private runtime group: {error}"),
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        match kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) => panic!("kill private runtime group: {error}"),
        }
    }
}

struct Driver {
    root: PathBuf,
    state: PathBuf,
    wrix: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}
fn text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 private fixture path")
}
fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("UTF-8 fixture output")
}
fn decode(output: &Output) -> TestResult<Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn read_json(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

impl Driver {
    fn new(root: &Path, wrix: &Path) -> TestResult<Self> {
        fs::create_dir_all(root.join("home"))?;
        fs::create_dir(root.join("empty-git-template"))?;
        let mut environment: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(key, _)| {
                let name = key.to_string_lossy();
                ![
                    "GIT_DIR",
                    "GIT_COMMON_DIR",
                    "GIT_WORK_TREE",
                    "GIT_INDEX_FILE",
                    "GIT_OBJECT_DIRECTORY",
                    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                    "GIT_CONFIG_PARAMETERS",
                    "DOLT_ROOT_HOST",
                    "DOLT_ROOT_PASSWORD",
                    "DOLT_CLI_PASSWORD",
                    "LOOM_INSIDE",
                    "IS_SANDBOX",
                ]
                .contains(&name.as_ref())
                    && !["BEADS_", "WRIX_BEADS_", "WRIX_SERVICE_IMAGE"]
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
            })
            .collect();
        let bin = root.join("bin");
        let path = std::env::join_paths(std::iter::once(bin.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("missing PATH")?),
        ))?;
        for (name, value) in [
            ("HOME", root.join("home").into_os_string()),
            ("XDG_STATE_HOME", root.join("state").into_os_string()),
            ("XDG_CACHE_HOME", root.join("cache").into_os_string()),
            ("WRIX_SHARED_FIXTURE", root.as_os_str().to_owned()),
            (
                "WRIX_SERVICE_PACKAGE_BINDING",
                root.join("binding.json").into_os_string(),
            ),
            (
                "WRIX_CONTAINER_RUNTIME",
                bin.join("podman").into_os_string(),
            ),
            ("WRIX_DOLT_TRANSPORT", OsString::from("unix")),
            ("PATH", path),
            ("WRIX_BIN", wrix.as_os_str().to_owned()),
            (
                "GIT_TEMPLATE_DIR",
                root.join("empty-git-template").into_os_string(),
            ),
        ] {
            environment.insert(name.into(), value);
        }
        Ok(Self {
            root: root.to_owned(),
            state: root.join(if cfg!(target_os = "macos") {
                "home/Library/Application Support/wrix"
            } else {
                "state/wrix"
            }),
            wrix: wrix.to_owned(),
            environment,
        })
    }
    async fn run(
        &self,
        args: &[&str],
        cwd: &Path,
        extra: &[(&str, &str)],
        success: bool,
    ) -> TestResult<Output> {
        let mut command = Command::new(args[0]);
        command
            .args(&args[1..])
            .current_dir(cwd)
            .env_clear()
            .envs(&self.environment)
            .envs(extra.iter().copied())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(120), command.output()).await??;
        if success && !output.status.success() {
            let log = self.root.join("service.log");
            let service = if log.exists() {
                fs::read_to_string(log)?
            } else {
                String::new()
            };
            assert!(
                output.status.success(),
                "{args:?}\n{}\n{}\n{service}",
                stdout(&output),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        Ok(output)
    }
    async fn cli(&self, workspace: &Path, args: &[&str], success: bool) -> TestResult<Output> {
        let arguments = std::iter::once(text(&self.wrix))
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        self.run(&arguments, workspace, &[], success).await
    }
    async fn sql(&self, client: &Value, query: &str, success: bool) -> TestResult<Output> {
        let credentials = &client["credentials"];
        let endpoints = read_json(&self.state.join("control/authority/role-endpoints.json"))?;
        let port = endpoints["ports"][0]
            .as_u64()
            .ok_or("missing native SQL port")?
            .to_string();
        let password = credentials["password"]
            .as_str()
            .ok_or("missing fixture password")?;
        self.run(
            &[
                "dolt",
                "--host",
                "127.0.0.1",
                "--port",
                &port,
                "--no-tls",
                "--password",
                password,
                "--user",
                credentials["user"].as_str().ok_or("missing SQL user")?,
                "--use-db",
                credentials["database"]
                    .as_str()
                    .ok_or("missing SQL database")?,
                "sql",
                "-r",
                "json",
                "-q",
                query,
            ],
            &self.root,
            &[("DOLT_CLI_PASSWORD", password)],
            success,
        )
        .await
    }
    async fn commit(&self, workspace: &Path, message: &str) -> TestResult {
        self.run(
            &[
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                message,
            ],
            workspace,
            &[],
            true,
        )
        .await?;
        Ok(())
    }
    async fn workflow(&self) -> TestResult {
        let workspaces = [self.root.join("first"), self.root.join("second")];
        for workspace in &workspaces {
            fs::create_dir(workspace)?;
            self.run(&["git", "init", "-q"], workspace, &[], true)
                .await?;
            fs::write(workspace.join("tracked"), "source content")?;
            self.run(&["git", "add", "tracked"], workspace, &[], true)
                .await?;
            self.commit(workspace, "initial").await?;
            let worktree = workspace.join(".git/beads-worktrees/beads");
            self.run(
                &["git", "worktree", "add", "-qb", "beads", text(&worktree)],
                workspace,
                &[],
                true,
            )
            .await?;
            let name = workspace.file_name().unwrap().to_str().unwrap();
            let upstream = self.root.join(format!("{name}-upstream.git"));
            self.run(
                &["git", "init", "--bare", "-q", text(&upstream)],
                &self.root,
                &[],
                true,
            )
            .await?;
            self.run(
                &["git", "remote", "add", "origin", text(&upstream)],
                workspace,
                &[],
                true,
            )
            .await?;
            self.run(
                &["git", "push", "-u", "origin", "beads"],
                &worktree,
                &[],
                true,
            )
            .await?;
            fs::create_dir(workspace.join(".beads"))?;
            fs::write(workspace.join(".beads/config.yaml"), "sync-branch: beads\n")?;
            fs::write(
                workspace.join(".beads/metadata.json"),
                "{\"database\":\"dolt\",\"backend\":\"dolt\",\"dolt_database\":\"beads\",\"dolt_mode\":\"embedded\"}\n",
            )?;
            let seed = self.root.join(format!("seed-{name}"));
            fs::create_dir(&seed)?;
            let beads = self
                .environment
                .get(std::ffi::OsStr::new("WRIX_REAL_BD_BIN"))
                .ok_or("missing native Beads")?;
            self.run(
                &[
                    beads.to_str().ok_or("non UTF-8 native Beads")?,
                    "init",
                    "--quiet",
                    "--skip-hooks",
                    "--skip-agents",
                    "--non-interactive",
                    "--prefix",
                    "fixture",
                ],
                &seed,
                &[("BEADS_DOLT_MODE", "embedded")],
                true,
            )
            .await?;
            let ignore = fs::read_to_string(seed.join(".beads/.gitignore"))?;
            fs::write(
                workspace.join(".beads/.gitignore"),
                format!("{ignore}\n/config.local.yaml\n/dolt/\n"),
            )?;
            self.run(
                &[
                    "git",
                    "add",
                    ".beads/metadata.json",
                    ".beads/config.yaml",
                    ".beads/.gitignore",
                ],
                workspace,
                &[],
                true,
            )
            .await?;
            self.commit(workspace, "portable Beads configuration")
                .await?;
            let source = workspace.join(".beads/dolt/beads");
            fs::create_dir(source.parent().unwrap())?;
            fs::rename(seed.join(".beads/embeddeddolt/fixture"), &source)?;
            for (key, value) in [
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
            ] {
                self.run(
                    &["dolt", "config", "--global", "--add", key, value],
                    &source,
                    &[],
                    true,
                )
                .await?;
            }
            self.run(&["dolt", "sql", "-q", "CREATE TABLE marker(id INT PRIMARY KEY); INSERT INTO marker VALUES(1); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','checkpoint'); CALL DOLT_BRANCH('retained'); CALL DOLT_TAG('retainedtag'); INSERT INTO marker VALUES(2); CALL DOLT_ADD('.'); INSERT INTO marker VALUES(3)"], &source, &[], true).await?;
        }
        let metadata = workspaces
            .iter()
            .map(|workspace| fs::read(workspace.join(".beads/metadata.json")))
            .collect::<std::io::Result<Vec<_>>>()?;
        self.cli(
            &workspaces[0],
            &["workspace", "acknowledge-scope", "--version", "1"],
            true,
        )
        .await?;
        let mut clients = Vec::new();
        for workspace in &workspaces {
            self.cli(workspace, &["service", "start", "--no-cache"], true)
                .await?;
            clients.push(decode(
                &self
                    .cli(workspace, &["service", "dolt", "client"], true)
                    .await?,
            )?);
        }
        assert_ne!(
            clients[0]["credentials"]["database"],
            clients[1]["credentials"]["database"]
        );
        for (workspace, before) in workspaces.iter().zip(&metadata) {
            assert_eq!(fs::read(workspace.join(".beads/metadata.json"))?, *before);
            self.run(
                &["git", "diff", "--exit-code", "--", ".beads"],
                workspace,
                &[],
                true,
            )
            .await?;
            assert_eq!(
                stdout(
                    &self
                        .run(
                            &["git", "status", "--porcelain", "--", ".beads"],
                            workspace,
                            &[],
                            true
                        )
                        .await?
                ),
                ""
            );
        }
        let endpoint = self.state.join("service-data/dolt/run/dolt.sock");
        fs::remove_file(&endpoint)?;
        drop(std::os::unix::net::UnixListener::bind(&endpoint)?);
        let registrations = fs::read_dir(self.state.join("host-control/dolt"))?
            .map(|entry| entry.map(|entry| entry.path().join("registration.json")))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| fs::read(&path).map(|bytes| (path, bytes)))
            .collect::<std::io::Result<BTreeMap<_, _>>>()?;
        let failed = self
            .cli(&workspaces[0], &["service", "start"], false)
            .await?;
        assert!(!failed.status.success());
        assert!(
            String::from_utf8_lossy(&failed.stderr)
                .contains("Unix endpoint is not accepting connections")
        );
        self.cli(&workspaces[0], &["service", "recover"], true)
            .await?;
        for (path, before) in &registrations {
            assert_eq!(fs::read(path)?, *before);
        }
        self.cli(&workspaces[0], &["service", "start"], true)
            .await?;
        let mut old: Value = serde_json::from_slice(&metadata[0])?;
        old["dolt_database"] = clients[0]["credentials"]["database"].clone();
        old["dolt_mode"] = json!("server");
        fs::write(
            workspaces[0].join(".beads/metadata.json"),
            serde_json::to_vec_pretty(&old)?,
        )?;
        self.cli(&workspaces[0], &["service", "start", "--no-cache"], true)
            .await?;
        assert_eq!(
            fs::read(workspaces[0].join(".beads/metadata.json"))?,
            metadata[0]
        );
        let workspace = &workspaces[0];
        let legacy_jsonl = workspace.join(".beads/issues.jsonl");
        fs::write(&legacy_jsonl, "legacy JSONL must not be imported\n")?;
        let hooks = workspace.join(".git/hooks");
        fs::create_dir_all(&hooks)?;
        for (name, script) in [
            (
                "post-checkout",
                "#!/usr/bin/env bash\nset -euo pipefail\nexec bd hooks run post-checkout \"$@\"\n",
            ),
            (
                "post-checkout.old",
                "#!/usr/bin/env bash\nset -euo pipefail\ntouch chained-hook-ran\n",
            ),
        ] {
            fs::write(hooks.join(name), script)?;
            fs::set_permissions(hooks.join(name), fs::Permissions::from_mode(0o755))?;
        }
        let checkout = self
            .run(
                &["git", "checkout", "-b", "after-managed-start"],
                workspace,
                &[],
                true,
            )
            .await?;
        let stderr = String::from_utf8_lossy(&checkout.stderr);
        assert!(!stderr.contains("JSONL import"), "{stderr}");
        assert!(!stderr.contains("no Dolt remote"), "{stderr}");
        assert!(workspace.join("chained-hook-ran").is_file());
        assert!(!workspace.join(".beads/dolt-server.pid").exists());
        assert_eq!(
            fs::read_to_string(&legacy_jsonl)?,
            "legacy JSONL must not be imported\n"
        );
        for client in &clients {
            assert_eq!(
                decode(
                    &self
                        .sql(
                            client,
                            "SELECT @@GLOBAL.secure_file_priv AS secure_file_priv",
                            true
                        )
                        .await?
                )?,
                json!({"rows":[{"secure_file_priv":"/dev/null"}]})
            );
            let rows = decode(
                &self
                    .sql(client, "SELECT * FROM marker ORDER BY id", true)
                    .await?,
            )?;
            let ids = rows["rows"]
                .as_array()
                .ok_or("missing SQL rows")?
                .iter()
                .map(|row| {
                    row["id"]
                        .as_i64()
                        .unwrap_or_else(|| row["id"].as_str().unwrap().parse().unwrap())
                })
                .collect::<Vec<_>>();
            assert_eq!(ids, [1, 2, 3]);
            assert_eq!(
                decode(
                    &self
                        .sql(
                            client,
                            "SELECT name FROM dolt_branches WHERE name = 'retained'",
                            true
                        )
                        .await?
                )?["rows"],
                json!([{"name":"retained"}])
            );
            assert_eq!(
                decode(
                    &self
                        .sql(
                            client,
                            "SELECT tag_name FROM dolt_tags WHERE tag_name = 'retainedtag'",
                            true
                        )
                        .await?
                )?["rows"],
                json!([{"tag_name":"retainedtag"}])
            );
            assert_eq!(
                decode(
                    &self
                        .sql(client, "SELECT staged FROM dolt_status", true)
                        .await?
                )?["rows"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert!(
                Path::new(client["git_common"].as_str().unwrap())
                    .starts_with(self.state.join("host-control/beads-git"))
            );
            self.sql(client, "INSERT INTO marker VALUES(4)", true)
                .await?;
        }
        let mut user_status = Vec::new();
        for workspace in &workspaces {
            user_status.push(
                self.run(
                    &["git", "status", "--porcelain", "--", ".beads"],
                    workspace,
                    &[],
                    true,
                )
                .await?
                .stdout,
            );
        }
        for workspace in &workspaces {
            let title = format!("scope-{}", workspace.file_name().unwrap().to_str().unwrap());
            self.run(
                &[
                    "bd", "create", "--title", &title, "--type", "task", "--json",
                ],
                workspace,
                &[],
                true,
            )
            .await?;
        }
        for (index, (workspace, before)) in workspaces.iter().zip(&metadata).enumerate() {
            let issues = decode(
                &self
                    .run(&["bd", "list", "--all", "--json"], workspace, &[], true)
                    .await?,
            )?;
            assert_eq!(
                issues
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|issue| issue["title"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [format!(
                    "scope-{}",
                    workspace.file_name().unwrap().to_str().unwrap()
                )]
            );
            assert_eq!(fs::read(workspace.join(".beads/metadata.json"))?, *before);
            self.run(
                &["git", "diff", "--exit-code", "--", ".beads"],
                workspace,
                &[],
                true,
            )
            .await?;
            assert_eq!(
                self.run(
                    &["git", "status", "--porcelain", "--", ".beads"],
                    workspace,
                    &[],
                    true
                )
                .await?
                .stdout,
                user_status[index]
            );
        }
        let peer = clients[1]["credentials"]["database"].as_str().unwrap();
        assert!(
            !self
                .sql(
                    &clients[0],
                    &format!("SELECT * FROM `{peer}`.marker"),
                    false
                )
                .await?
                .status
                .success()
        );
        for user in ["root", "wrixadmin"] {
            for password in ["", "wrong-private-password"] {
                let mut bad = clients[0].clone();
                bad["credentials"]["user"] = json!(user);
                bad["credentials"]["password"] = json!(password);
                assert!(
                    !self
                        .sql(&bad, "SELECT CURRENT_USER()", false)
                        .await?
                        .status
                        .success()
                );
            }
        }
        self.cli(&workspaces[0], &["service", "stop", "--no-cache"], true)
            .await?;
        assert_eq!(
            decode(&self.sql(&clients[1], "SELECT * FROM marker", true).await?)?["rows"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert!(
            !self
                .sql(&clients[0], "SELECT * FROM marker", false)
                .await?
                .status
                .success()
        );
        self.cli(&workspaces[0], &["service", "start", "--no-cache"], true)
            .await?;
        let renewed = decode(
            &self
                .cli(&workspaces[0], &["service", "dolt", "client"], true)
                .await?,
        )?;
        assert_eq!(
            decode(&self.sql(&renewed, "SELECT * FROM marker", true).await?)?["rows"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let calls = fs::read_to_string(self.root.join("runtime.log"))?
            .lines()
            .map(serde_json::from_str::<Vec<String>>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let runs = calls
            .iter()
            .filter(|call| call[0] == "run")
            .collect::<Vec<_>>();
        assert_eq!(runs.len(), 2, "{runs:?}");
        let mut names = runs
            .iter()
            .map(|call| call[call.iter().position(|arg| arg == "--name").unwrap() + 1].as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["wrix-gateway-1", "wrix-host-service"]);
        assert!(!calls.iter().any(|call| call[0] == "rm"), "{calls:?}");
        assert!(
            runs[0]
                .iter()
                .any(|arg| arg == "wrix.dolt.shared=native-v1")
        );
        for (workspace, client) in workspaces.iter().zip(&clients) {
            let guest = decode(
                &self
                    .cli(workspace, &["service", "dolt", "sandbox-client"], true)
                    .await?,
            )?;
            assert!(
                Path::new(guest["git_common"].as_str().unwrap())
                    .starts_with(self.state.join("service-data/dolt/namespaces"))
            );
            assert!(
                !Path::new(client["git_common"].as_str().unwrap())
                    .starts_with(self.state.join("service-data"))
            );
            assert_ne!(guest["git_common"], client["git_common"]);
        }
        assert!(!runs[0].iter().any(|arg| arg.contains("host-control")
            || arg.contains("/first/.git")
            || arg.contains("/second/.git")));
        println!(
            "public CLI: one native shared backend; retained rows/branches, scoped writes, peer/admin denial, retire/reactivate preserved peer"
        );
        Ok(())
    }
}

// The owning workflow runs in Linux user/mount/network namespaces. Darwin's
// killpg(0) observation of an unreaped group has different permission semantics.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn cleanup_retires_only_the_private_recorded_runtime_group() -> TestResult {
    let root = tempfile::tempdir()?;
    let mut child = Command::new("sleep")
        .arg("120")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let identifier = child.id().ok_or("missing private runtime PID")?;
    fs::write(
        root.path().join("runtime.json"),
        serde_json::to_vec(&json!({"pid":identifier}))?,
    )?;
    drop(RuntimeCleanup(root.path().to_owned()));
    let status = child.wait().await?;
    assert!(!status.success());
    let group = Pid::from_raw(i32::try_from(identifier)?).ok_or("missing private runtime PID")?;
    assert_eq!(test_kill_process_group(group), Err(rustix::io::Errno::SRCH));
    Ok(())
}

#[tokio::test]
async fn driver_preserves_failed_command_output_without_claiming_success() -> TestResult {
    let root = tempfile::tempdir()?;
    let driver = Driver::new(root.path(), Path::new("/unused-private-wrix"))?;
    let output = driver
        .run(
            &[
                "bash",
                "-c",
                "printf 'native stdout'; printf 'native stderr' >&2; exit 23",
            ],
            root.path(),
            &[],
            false,
        )
        .await?;
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"native stdout");
    assert_eq!(output.stderr, b"native stderr");
    assert_eq!(
        driver.environment.get(std::ffi::OsStr::new("HOME")),
        Some(&root.path().join("home").into_os_string())
    );
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "DOLT_ROOT_HOST",
        "DOLT_ROOT_PASSWORD",
        "DOLT_CLI_PASSWORD",
        "LOOM_INSIDE",
        "IS_SANDBOX",
    ] {
        assert!(!driver.environment.contains_key(std::ffi::OsStr::new(name)));
    }
    assert!(!driver.environment.keys().any(|name| {
        ["BEADS_", "WRIX_BEADS_", "WRIX_SERVICE_IMAGE"]
            .iter()
            .any(|prefix| name.to_string_lossy().starts_with(prefix))
    }));
    Ok(())
}

#[test]
fn public_driver_refuses_unmarked_roots_before_creating_state() -> TestResult {
    let root = tempfile::tempdir()?;
    assert!(run(root.path(), Path::new("/unused-private-wrix")).is_err());
    assert_eq!(fs::read_dir(root.path())?.count(), 0);
    Ok(())
}

pub fn run(root: &Path, wrix: &Path) -> TestResult {
    if fs::read(root.join(".native-public-cli-driver"))? != b"wrix-native-public-cli-v1\n"
        || fs::metadata(root)?.permissions().mode() & 0o077 != 0
    {
        return Err("public CLI driver requires its owned private fixture root".into());
    }
    let _cleanup = RuntimeCleanup(root.to_owned());
    let driver = Driver::new(root, wrix)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = driver.workflow() => result,
                _ = terminate.recv() => Err("native public CLI fixture deadline reached".into()),
            }
        })
}
