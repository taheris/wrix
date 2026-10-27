use std::{
    fs,
    net::TcpListener,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

use super::{Credentials, Error, Manager, Phase, Sync, literal};
use crate::control::{Access, Client, Controller, Namespace, Operation, Permission, Policy, Reply};
use wrix_core::{git::Branch, path::WorkspaceHash};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

struct Fixture {
    _server: Server,
    root: tempfile::TempDir,
    source: PathBuf,
    runtime: tokio::runtime::Runtime,
    controller: Controller,
    namespace: Namespace,
    port: u16,
}

fn native(directory: &Path, arguments: &[&str]) -> TestResult<String> {
    let result = Command::new("dolt")
        .args(arguments)
        .current_dir(directory)
        .env("HOME", directory.parent().unwrap())
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .env_remove("DOLT_CLI_PASSWORD")
        .stdin(Stdio::null())
        .output()?;
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    Ok(String::from_utf8(result.stdout)?)
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix("wds-").tempdir()?;
        let source = root.path().join("original");
        let data = root.path().join("service-data/dolt/databases");
        let socket = root.path().join("service-data/dolt/run/management.sock");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&data)?;
        fs::create_dir_all(socket.parent().unwrap())?;
        native(
            &source,
            &[
                "init",
                "--name",
                "Fixture",
                "--email",
                "fixture@example.invalid",
            ],
        )?;
        native(
            &source,
            &[
                "sql",
                "-q",
                "CREATE TABLE marker(id INT PRIMARY KEY); INSERT INTO marker VALUES(1); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture'); CALL DOLT_BRANCH('other'); CALL DOLT_TAG('checkpoint'); INSERT INTO marker VALUES(2); CALL DOLT_ADD('.'); INSERT INTO marker VALUES(3)",
            ],
        )?;
        native(
            root.path(),
            &[
                "--data-dir",
                data.to_str().unwrap(),
                "sql",
                "-q",
                "CREATE DATABASE controller; CREATE DATABASE peer; USE peer; CREATE TABLE marker(id INT PRIMARY KEY); INSERT INTO marker VALUES(42)",
            ],
        )?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let log = fs::File::create(root.path().join("server.log"))?;
        let config = root.path().join("server.yaml");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "data_dir": data,
                "listener": { "host": "127.0.0.1", "port": port, "socket": socket },
                "system_variables": { "secure_file_priv": "/dev/null" }
            }))?,
        )?;
        let mut server = Server(
            Command::new("dolt")
                .args(["sql-server", "--config", config.to_str().unwrap()])
                .current_dir(root.path())
                .env("HOME", root.path())
                .env_remove("DOLT_ROOT_HOST")
                .env_remove("DOLT_ROOT_PASSWORD")
                .env_remove("DOLT_CLI_PASSWORD")
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while !socket.exists() {
            assert!(
                server.0.try_wait()?.is_none() && Instant::now() < deadline,
                "{}",
                fs::read_to_string(root.path().join("server.log"))?
            );
            thread::sleep(Duration::from_millis(20));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let controller = {
            let _enter = runtime.enter();
            Controller::bind(&root.path().join("authority"))?
        };
        let namespace = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64))?);
        let client = Client::open(&root.path().join("authority"))?;
        runtime.block_on(async {
            let (serve, reply) = tokio::join!(
                controller.serve_one(),
                client.request(Operation::Register {
                    namespace: namespace.clone(),
                    policy: Policy {
                        dolt: Access::Allowed,
                        private_cache_read: Access::Denied,
                        common_cache_read: Access::Denied,
                        egress: Access::Denied,
                        compiler: Access::Denied
                    }
                })
            );
            serve.unwrap();
            assert_eq!(reply.unwrap(), Reply::Applied);
        });
        Ok(Self {
            root,
            source,
            _server: server,
            runtime,
            controller,
            namespace,
            port,
        })
    }

    fn manager<'a>(
        &self,
        authorization: &'a crate::control::Authorization<'a>,
        branch: Branch,
    ) -> Result<Manager<'a>, Error> {
        Manager::at(
            authorization,
            branch,
            self.root.path(),
            std::num::NonZeroU16::new(self.port).unwrap(),
            "",
        )
    }

    fn backup(&self, manager: &Manager<'_>) -> TestResult {
        native(&self.source, &["backup", "sync-url", &manager.backup_url()])?;
        Ok(())
    }

    fn rows(&self, query: &str) -> TestResult<Value> {
        let text = native(&self.source, &["sql", "-r", "json", "-q", query])?;
        Ok(serde_json::from_str::<Value>(&text)?["rows"].clone())
    }

    fn tenant(&self, credentials: &Credentials, query: &str) -> TestResult<std::process::Output> {
        Ok(Command::new("dolt")
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &self.port.to_string(),
                "--no-tls",
                "--user",
                credentials.user(),
                "--use-db",
                credentials.database(),
                "sql",
                "-r",
                "json",
                "-q",
                query,
            ])
            .env("DOLT_CLI_PASSWORD", credentials.password())
            .current_dir(self.root.path())
            .stdin(Stdio::null())
            .output()?)
    }
}

#[test]
fn live_restore_preserves_history_refs_staged_and_unstaged_roots_without_restarting_peer()
-> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("team/beads")?)?;
    fixture.backup(&manager)?;
    manager.register()?;
    for query in [
        "SELECT commit_hash FROM dolt_log",
        "SELECT table_name, CAST(staged AS CHAR) AS staged, status FROM dolt_status ORDER BY table_name, staged",
        "SELECT name,hash FROM dolt_branches ORDER BY name",
        "SELECT tag_name,tag_hash FROM dolt_tags ORDER BY tag_name",
    ] {
        assert_eq!(
            Value::Array(manager.admin(Some(&manager.database()), query)?),
            fixture.rows(query)?,
            "{query}"
        );
    }
    let rows = manager.admin(
        Some(&manager.database()),
        "SELECT id FROM marker ORDER BY id",
    )?;
    assert_eq!(
        rows.iter()
            .map(|row| row["id"].to_string().replace('"', ""))
            .collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
    assert_eq!(
        manager.admin(Some("peer"), "SELECT id FROM marker")?[0]["id"],
        "42"
    );
    assert_eq!(
        fixture.rows("SELECT COUNT(*) AS n FROM marker")?[0]["n"]
            .to_string()
            .replace('"', ""),
        "3"
    );
    drop(manager);
    drop(authorization);
    drop(fixture.controller);
    drop(fixture.runtime);
    Ok(())
}

#[test]
fn scoped_credentials_allow_writes_and_commits_but_deny_peer_and_admin_deputy_operations()
-> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    let credentials = manager.register()?;
    let output = fixture.tenant(
        &credentials,
        "INSERT INTO marker VALUES(4); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','tenant write')",
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let secret = fixture.root.path().join("peer-secret");
    fs::write(&secret, "must not be readable")?;
    let output = fixture.tenant(
        &credentials,
        &format!(
            "SELECT LOAD_FILE({}) AS contents",
            literal(secret.to_str().unwrap())
        ),
    )?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["rows"][0]["contents"],
        Value::Null
    );
    for query in [
        "SET GLOBAL secure_file_priv = ''",
        "SET SESSION secure_file_priv = ''",
        "SELECT * FROM peer.marker",
        "GRANT ALL PRIVILEGES ON *.* TO 'attacker'@'%'",
        "CALL DOLT_REMOTE('add','escape','file:///tmp/escape')",
        "CALL DOLT_PUSH('file:///tmp/escape','main')",
        "CALL DOLT_PULL('file:///tmp/escape','main')",
        "CALL DOLT_BACKUP('sync-url','file:///tmp/escape')",
        "CALL DOLT_CLONE('file:///tmp/escape','escape')",
    ] {
        let output = fixture.tenant(&credentials, query)?;
        assert!(
            !output.status.success(),
            "tenant unexpectedly admitted {query}"
        );
    }
    assert!(!format!("{credentials:?}").contains(credentials.password()));
    Ok(())
}

#[test]
fn trusted_sync_uses_only_fixed_namespace_remote_and_repairs_a_stale_origin() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    let credentials = manager.register()?;
    manager.admin(Some(&manager.database()), "CALL DOLT_REMOTE('remove','origin'); CALL DOLT_REMOTE('add','origin','file:///tmp/must-not-be-used')")?;
    manager.sync(Sync::Push)?;
    let remote = manager.admin(
        Some(&manager.database()),
        "SELECT url FROM dolt_remotes WHERE name='origin'",
    )?;
    assert_eq!(remote[0]["url"], manager.remote_url());
    let output = fixture.tenant(
        &credentials,
        "INSERT INTO marker VALUES(4); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','scoped commit')",
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    manager.sync(Sync::Push)?;
    manager.sync(Sync::Pull)?;
    Ok(())
}

#[test]
fn repeated_registration_keeps_password_and_never_reimports_the_old_backup() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    let credentials = manager.register()?;
    manager.admin(Some(&manager.database()), "INSERT INTO marker VALUES(4)")?;
    let repeated = manager.register()?;
    assert_eq!(credentials.password(), repeated.password());
    assert_eq!(
        manager.admin(
            Some(&manager.database()),
            "SELECT COUNT(*) AS n FROM marker"
        )?[0]["n"],
        "4"
    );
    Ok(())
}

#[test]
fn interrupted_restore_refuses_to_overwrite_an_uncertain_database() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    manager.register()?;
    let mut record = manager.read_record()?.unwrap();
    record.phase = Phase::Restoring;
    manager.write_record(&record)?;
    assert!(matches!(manager.register(), Err(Error::Incomplete)));
    assert_eq!(
        manager.admin(
            Some(&manager.database()),
            "SELECT COUNT(*) AS n FROM marker"
        )?[0]["n"],
        "3"
    );
    Ok(())
}

#[test]
fn absent_backup_or_registered_database_never_creates_an_empty_replacement() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    assert!(matches!(manager.register(), Err(Error::Incomplete)));
    assert!(!manager.database_exists()?);
    fixture.backup(&manager)?;
    manager.register()?;
    manager.admin(None, &format!("DROP DATABASE `{}`", manager.database()))?;
    assert!(matches!(manager.register(), Err(Error::Incomplete)));
    assert!(!manager.database_exists()?);
    Ok(())
}

#[test]
fn native_source_cutover_keeps_writer_lock_and_resumes_after_checkpoint_rename() -> TestResult {
    use fs2::FileExt;
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    let before = fixture.rows("SELECT name,hash FROM dolt_branches ORDER BY name")?;
    fs::write(
        fixture.source.join(".dolt/sql-server.lock"),
        "untrusted stale server connection metadata",
    )?;
    // Simulate power loss after journal+rename, before export.
    super::atomic_json(
        &manager.root.join("source.json"),
        &super::Source {
            source: fixture.source.clone(),
            phase: super::SourcePhase::Moving,
        },
    )?;
    fs::rename(&fixture.source, manager.root.join("source-checkpoint"))?;
    manager.prepare_source(&fixture.source)?;
    assert!(!fixture.source.exists());
    let contender = fs::File::options()
        .read(true)
        .write(true)
        .open(manager.root.join("source-checkpoint/.dolt/noms/LOCK"))?;
    assert!(
        contender.try_lock_exclusive().is_err(),
        "native source lock must survive registration"
    );
    assert!(
        !manager
            .root
            .join("source-snapshot/.dolt/sql-server.lock")
            .exists(),
        "native export must not inherit auto-connection metadata"
    );
    let private_manifest = manager.root.join("source-snapshot/.dolt/noms/manifest");
    let original_manifest = manager.root.join("source-checkpoint/.dolt/noms/manifest");
    assert_ne!(
        fs::metadata(&private_manifest)?.ino(),
        fs::metadata(&original_manifest)?.ino()
    );
    // A legacy worker may retain its old directory descriptor after rename.
    // Mutating that old graph must not influence privileged native restore.
    let retained = fs::File::open(manager.root.join("source-checkpoint/.dolt"))?;
    rustix::fs::renameat(&retained, "noms", &retained, "retained-noms")?;
    rustix::fs::symlinkat(
        fixture
            .root
            .path()
            .join("service-data/dolt/databases/peer/.dolt/noms"),
        &retained,
        "noms",
    )?;
    let credentials = manager.register()?;
    assert_eq!(
        Value::Array(manager.admin(
            Some(credentials.database()),
            "SELECT name,hash FROM dolt_branches ORDER BY name"
        )?),
        before
    );
    assert_eq!(
        manager.admin(Some("peer"), "SELECT id FROM marker")?[0]["id"],
        "42"
    );
    assert_eq!(
        manager.admin(
            Some(credentials.database()),
            "SELECT COUNT(*) AS n FROM marker"
        )?[0]["n"],
        "3"
    );
    assert_eq!(
        manager
            .admin(
                Some(credentials.database()),
                "SELECT staged FROM dolt_status"
            )?
            .len(),
        2
    );
    drop(manager);
    contender.try_lock_exclusive()?;
    Ok(())
}

#[test]
fn native_source_with_active_writer_is_rejected_before_journal_or_rename() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let socket = fixture.root.path().join("source-writer.sock");
    let log = fs::File::create(fixture.root.path().join("writer.log"))?;
    let mut writer = Server(
        Command::new("dolt")
            .args([
                "sql-server",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--socket",
                socket.to_str().unwrap(),
            ])
            .current_dir(&fixture.source)
            .env("HOME", fixture.root.path())
            .env_remove("DOLT_ROOT_HOST")
            .env_remove("DOLT_ROOT_PASSWORD")
            .env_remove("DOLT_CLI_PASSWORD")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(writer.0.try_wait()?.is_none() && Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        manager.prepare_source(&fixture.source),
        Err(Error::SourceBusy { .. })
    ));
    assert!(fixture.source.join(".dolt/noms/LOCK").is_file());
    assert!(!manager.root.join("source.json").exists());
    assert!(!manager.data.join("registration-backup").exists());
    Ok(())
}

fn multi_database_source() -> TestResult<(Fixture, PathBuf)> {
    let mut fixture = Fixture::new()?;
    let workspace = fixture.root.path().join("workspace");
    let data = workspace.join(".beads/dolt");
    fs::create_dir_all(&data)?;
    fs::write(
        workspace.join(".beads/metadata.json"),
        r#"{"backend":"dolt","dolt_database":"lm","dolt_mode":"server"}"#,
    )?;
    native(
        &data,
        &[
            "init",
            "--name",
            "Fixture",
            "--email",
            "fixture@example.invalid",
        ],
    )?;
    native(
        &data,
        &[
            "sql",
            "-q",
            "CREATE TABLE root_only(id INT PRIMARY KEY); INSERT INTO root_only VALUES(77)",
        ],
    )?;
    let named = data.join("lm");
    fs::rename(&fixture.source, &named)?;
    fixture.source = named;
    let graph = data.join("graph");
    fs::create_dir(&graph)?;
    native(
        &graph,
        &[
            "init",
            "--name",
            "Fixture",
            "--email",
            "fixture@example.invalid",
        ],
    )?;
    native(
        &graph,
        &[
            "sql",
            "-q",
            "CREATE TABLE graph_only(id INT PRIMARY KEY); INSERT INTO graph_only VALUES(88)",
        ],
    )?;
    Ok((fixture, workspace))
}

#[test]
fn multi_database_adoption_restores_only_configured_lm_and_preserves_root_and_graph() -> TestResult
{
    let (fixture, workspace) = multi_database_source()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    {
        let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
        manager.prepare_workspace_source(&workspace)?;
        let credentials = manager.register()?;
        assert_eq!(
            manager.query(
                Some(&credentials),
                Some(credentials.database()),
                "SELECT * FROM marker ORDER BY id"
            )?,
            vec![
                serde_json::json!({"id":"1"}),
                serde_json::json!({"id":"2"}),
                serde_json::json!({"id":"3"})
            ]
        );
        for query in [
            "SELECT COUNT(*) AS n FROM dolt_branches WHERE name='other'",
            "SELECT COUNT(*) AS n FROM dolt_tags WHERE tag_name='checkpoint'",
        ] {
            assert_eq!(
                manager.query(Some(&credentials), Some(credentials.database()), query)?,
                vec![serde_json::json!({"n":"1"})]
            );
        }
        assert_eq!(
            manager.source_record()?.unwrap().source,
            workspace.join(".beads/dolt/lm")
        );
        assert!(!workspace.join(".beads/dolt/lm").exists());
        assert!(workspace.join(".beads/dolt/.dolt/noms").is_dir());
        assert!(workspace.join(".beads/dolt/graph/.dolt/noms").is_dir());
    }
    assert!(
        native(
            &workspace.join(".beads/dolt"),
            &["sql", "-r", "json", "-q", "SELECT * FROM root_only"]
        )?
        .contains("77")
    );
    assert!(
        native(
            &workspace.join(".beads/dolt/graph"),
            &["sql", "-r", "json", "-q", "SELECT * FROM graph_only"]
        )?
        .contains("88")
    );
    Ok(())
}

#[test]
fn multi_database_parent_writer_fences_migration_before_any_source_move() -> TestResult {
    use fs2::FileExt;
    let (fixture, workspace) = multi_database_source()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    let lock = fs::File::open(workspace.join(".beads/dolt/.dolt/noms/LOCK"))?;
    lock.lock_exclusive()?;
    assert!(
        matches!(manager.prepare_workspace_source(&workspace), Err(Error::SourceBusy { path }) if path == workspace.join(".beads/dolt"))
    );
    assert!(fixture.source.join(".dolt/noms").is_dir());
    assert!(!manager.root.join("source.json").exists());
    assert!(!manager.data.join("registration-backup").exists());
    Ok(())
}

#[test]
fn source_symlinks_cannot_adopt_peer_native_storage() -> TestResult {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    let forged = fixture.root.path().join("forged");
    fs::create_dir(&forged)?;
    symlink(&fixture.source, forged.join("database"))?;
    assert!(matches!(
        manager.prepare_source(&forged.join("database")),
        Err(Error::UnsafePath)
    ));
    symlink(fixture.source.join(".dolt"), forged.join(".dolt"))?;
    assert!(matches!(
        manager.prepare_source(&forged),
        Err(Error::UnsafePath)
    ));
    fs::remove_file(forged.join(".dolt"))?;
    fs::create_dir(forged.join(".dolt"))?;
    symlink(fixture.source.join(".dolt/noms"), forged.join(".dolt/noms"))?;
    assert!(matches!(
        manager.prepare_source(&forged),
        Err(Error::UnsafePath)
    ));
    assert!(!manager.root.join("source.json").exists());
    assert!(!manager.data.join("registration-backup").exists());
    assert_eq!(
        fixture.rows("SELECT COUNT(*) AS n FROM marker")?[0]["n"]
            .to_string()
            .replace('"', ""),
        "3"
    );
    Ok(())
}

#[test]
fn readiness_reads_only_selected_ready_native_registrations_without_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    let original = manager.register()?;
    let path = manager.root.join("registration.json");
    let before = fs::read(&path)?;
    let credentials = super::readiness_credentials(
        fixture.root.path(),
        std::slice::from_ref(&fixture.namespace),
    )?;
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].database(), original.database());
    assert_eq!(credentials[0].user(), original.user());
    assert_eq!(credentials[0].password(), original.password());
    assert!(super::readiness_credentials(fixture.root.path(), &[])?.is_empty());
    assert_eq!(fs::read(path)?, before);
    let missing = Namespace::from(&WorkspaceHash::parse(&"b".repeat(64))?);
    assert!(
        super::readiness_credentials(fixture.root.path(), std::slice::from_ref(&missing)).is_err()
    );
    assert!(
        !fixture
            .root
            .path()
            .join("host-control/dolt")
            .join(missing.as_str())
            .exists()
    );
    Ok(())
}

#[test]
fn readiness_rejects_incomplete_native_registration_without_reprovisioning() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    let credentials = manager.register()?;
    for phase in [Phase::Restoring, Phase::Imported] {
        let mut record = manager.read_record()?.unwrap();
        record.phase = phase;
        manager.write_record(&record)?;
        assert!(matches!(
            super::readiness_credentials(
                fixture.root.path(),
                std::slice::from_ref(&fixture.namespace)
            ),
            Err(Error::Incomplete)
        ));
        assert!(
            fixture
                .tenant(&credentials, "SELECT COUNT(*) FROM marker")?
                .status
                .success()
        );
    }
    Ok(())
}

#[test]
fn readiness_and_manager_share_native_credential_identity_validation() -> TestResult {
    let fixture = Fixture::new()?;
    let authorization = fixture
        .controller
        .authorize(&fixture.namespace, Permission::Sql)?;
    let manager = fixture.manager(&authorization, Branch::parse("beads")?)?;
    fixture.backup(&manager)?;
    manager.register()?;
    let path = manager.root.join("registration.json");
    let original = fs::read(&path)?;
    for (field, value) in [
        ("database", "peer"),
        ("user", "wrixadmin"),
        ("password", "invalid"),
    ] {
        let mut record: Value = serde_json::from_slice(&original)?;
        record["credentials"][field] = Value::String(value.into());
        super::atomic_json(&path, &record)?;
        assert!(matches!(manager.read_record(), Err(Error::Incomplete)));
        assert!(matches!(
            super::readiness_credentials(
                fixture.root.path(),
                std::slice::from_ref(&fixture.namespace)
            ),
            Err(Error::Incomplete)
        ));
    }
    Ok(())
}

#[test]
fn sql_literals_preserve_quotes_and_backslashes() {
    assert_eq!(literal("a'b\\c"), "'a''b\\\\c'");
}
