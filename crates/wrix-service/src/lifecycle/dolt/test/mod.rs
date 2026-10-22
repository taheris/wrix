use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{DoltTransport, probe_tcp, probe_tcp_with_client, server_command};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    data: PathBuf,
    socket_dir: PathBuf,
    port: u16,
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix("wd-").tempdir()?;
        let data = root.path().join("data with 'quotes'");
        let socket_dir = root.path().join("run");
        fs::create_dir_all(&data)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            root,
            data,
            socket_dir,
            port,
        })
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env_remove("DOLT_ROOT_HOST")
            .env_remove("DOLT_ROOT_PASSWORD")
            .env_remove("DOLT_CLI_PASSWORD")
            .stdin(Stdio::null());
        command
    }

    fn sql(&self, query: &str) -> TestResult<String> {
        output(
            self.command("dolt")
                .arg("--data-dir")
                .arg(&self.data)
                .args(["sql", "-r", "csv", "-q", query]),
        )
    }

    fn connect(&self, user: &str, password: &str, query: &str) -> TestResult<String> {
        output(&mut self.connection(user, password, query))
    }

    fn connection(&self, user: &str, password: &str, query: &str) -> Command {
        let mut command = self.command("dolt");
        command.args([
            "--host",
            "127.0.0.1",
            "--port",
            &self.port.to_string(),
            "--no-tls",
            "--user",
            user,
            "--password",
            password,
            "sql",
            "-r",
            "csv",
            "-q",
            query,
        ]);
        command
    }

    fn scoped(&self) -> TestResult<Server> {
        self.sql(
            "CREATE DATABASE tenant; CREATE USER 'tenant'@'%' IDENTIFIED BY 'fixture'; \
            GRANT ALL ON tenant.* TO 'tenant'@'%';",
        )?;
        self.start(DoltTransport::Tcp)
    }

    fn start(&self, transport: DoltTransport) -> TestResult<Server> {
        let log = fs::File::create(self.root.path().join("server.log"))?;
        let mut server = Server(
            self.command("sh")
                .args([
                    "-c",
                    &server_command(transport, &self.data, &self.socket_dir, self.port),
                ])
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.socket_dir.join("dolt.sock").exists() {
            assert!(
                server.0.try_wait()?.is_none() && Instant::now() < deadline,
                "Dolt failed to start: {}",
                fs::read_to_string(self.root.path().join("server.log"))?
            );
            thread::sleep(Duration::from_millis(20));
        }
        Ok(server)
    }
}

fn output(command: &mut Command) -> TestResult<String> {
    let result = command.output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?)
}

#[test]
fn fresh_tcp_service_provisions_a_forwarded_root_login() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.start(DoltTransport::Tcp)?;
    assert_eq!(
        fixture
            .connect("root", "", "SELECT CURRENT_USER() AS account")?
            .trim(),
        "account\nroot@%"
    );
    fixture.connect("root", "", "CREATE DATABASE lm; USE lm; CREATE TABLE marker (id INT PRIMARY KEY); INSERT INTO marker VALUES (1)")?;
    probe_tcp(fixture.port, Duration::from_secs(2))?;
    Ok(())
}

#[test]
fn tcp_bootstrap_repairs_persisted_localhost_grants_without_losing_state() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.sql("CREATE DATABASE lm; USE lm; CREATE TABLE marker (id INT PRIMARY KEY); INSERT INTO marker VALUES (42); \
        CREATE USER 'reader'@'%' IDENTIFIED BY 'fixture-password'; GRANT SELECT ON lm.* TO 'reader'@'%';")?;
    assert!(
        !fixture
            .sql("SELECT Host FROM mysql.user WHERE User = 'root'")?
            .contains('%')
    );
    let grants = fixture.sql("SHOW GRANTS FOR 'reader'@'%'")?;
    let privileges = fixture.data.join(".doltcfg/privileges.db");
    assert!(privileges.is_file());

    for _ in 0..2 {
        let server = fixture.start(DoltTransport::Tcp)?;
        assert_eq!(
            fixture
                .connect("reader", "fixture-password", "SELECT id FROM lm.marker")?
                .trim(),
            "id\n42"
        );
        assert_eq!(
            fixture.connect("root", "", "SHOW GRANTS FOR 'reader'@'%'")?,
            grants
        );
        assert_eq!(
            fixture
                .connect(
                    "root",
                    "",
                    "SELECT COUNT(*) AS n FROM mysql.user WHERE User='root' AND Host='%'"
                )?
                .trim(),
            "n\n1"
        );
        drop(server);
        fs::remove_file(fixture.socket_dir.join("dolt.sock"))?;
    }
    Ok(())
}

#[test]
fn tcp_bootstrap_does_not_reset_an_existing_root_password() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.sql("CREATE USER 'root'@'%' IDENTIFIED BY 'existing-password'; GRANT ALL PRIVILEGES ON *.* TO 'root'@'%';")?;
    let _server = fixture.start(DoltTransport::Tcp)?;
    assert_eq!(
        fixture
            .connect(
                "root",
                "existing-password",
                "SELECT CURRENT_USER() AS account"
            )?
            .trim(),
        "account\nroot@%"
    );
    assert!(probe_tcp(fixture.port, Duration::from_secs(2)).is_err());
    Ok(())
}

#[test]
fn unix_service_does_not_add_a_remote_root_login() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.start(DoltTransport::UnixSocket)?;
    assert_eq!(
        fixture
            .connect(
                "root",
                "",
                "SELECT Host FROM mysql.user WHERE User = 'root'"
            )?
            .trim(),
        "Host\nlocalhost"
    );
    Ok(())
}

#[test]
fn sql_probe_rejects_a_reachable_server_with_invalid_credentials() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.sql("ALTER USER 'root'@'localhost' IDENTIFIED BY 'private-password';")?;
    let _server = fixture.start(DoltTransport::UnixSocket)?;
    let error = probe_tcp(fixture.port, Duration::from_secs(2)).unwrap_err();
    assert!(error.to_string().contains("Access denied"), "{error}");
    Ok(())
}

#[test]
fn sql_probe_honors_a_caller_budget_longer_than_one_second() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir()?;
    let client = root.path().join("dolt");
    fs::write(&client, "#!/bin/sh\nsleep 1.1\nexit 0\n")?;
    fs::set_permissions(&client, fs::Permissions::from_mode(0o755))?;
    probe_tcp_with_client(12345, Duration::from_secs(3), &client)?;
    Ok(())
}

#[test]
fn sql_probe_zero_budget_does_not_spawn_a_client() {
    let error = probe_tcp_with_client(12345, Duration::ZERO, Path::new("/missing-dolt-client"))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn sql_probe_times_out_when_a_tcp_listener_never_speaks_mysql() -> TestResult {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let start = Instant::now();
    let error = probe_tcp(listener.local_addr()?.port(), Duration::from_millis(100)).unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    Ok(())
}

#[test]
fn scoped_clients_cannot_read_server_files_with_load_file() -> TestResult {
    let fixture = Fixture::new()?;
    let secret = fixture.root.path().join("peer-secret");
    fs::write(&secret, "peer authority must not reach SQL clients")?;
    let _server = fixture.scoped()?;
    assert_eq!(
        fixture
            .connect(
                "tenant",
                "fixture",
                &format!(
                    "SELECT LOAD_FILE('{}') IS NULL AS blocked",
                    secret.display()
                )
            )?
            .trim(),
        "blocked\n1"
    );
    fixture.connect(
        "tenant",
        "fixture",
        "USE tenant; CREATE TABLE marker (id INT PRIMARY KEY); INSERT INTO marker VALUES (42)",
    )?;
    assert_eq!(
        fixture
            .connect("tenant", "fixture", "SELECT id FROM tenant.marker")?
            .trim(),
        "id\n42"
    );
    Ok(())
}

#[test]
fn scoped_clients_cannot_export_server_files() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.scoped()?;
    let destination = fixture.root.path().join("unexpected-file");
    for path in [
        destination.display().to_string(),
        format!("/dev/null/../..{}", destination.display()),
    ] {
        let output = fixture
            .connection(
                "tenant",
                "fixture",
                &format!("SELECT 'unexpected' INTO OUTFILE '{path}'"),
            )
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("secure-file-priv"));
        assert!(!destination.exists());
    }
    Ok(())
}

#[test]
fn sql_clients_cannot_reenable_server_filesystem_access() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.scoped()?;
    for scope in ["SESSION", "GLOBAL", "PERSIST", "PERSIST_ONLY"] {
        let output = fixture
            .connection(
                "tenant",
                "fixture",
                &format!("SET {scope} secure_file_priv = ''"),
            )
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("read only variable"));
    }
    assert_eq!(
        fixture
            .connect("tenant", "fixture", "SELECT @@secure_file_priv AS boundary")?
            .trim(),
        "boundary\n/dev/null"
    );
    Ok(())
}

#[test]
fn scoped_clients_cannot_import_server_files() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.scoped()?;
    fixture.connect(
        "tenant",
        "fixture",
        "USE tenant; CREATE TABLE imported (contents TEXT)",
    )?;
    let secret = fixture.root.path().join("peer-export");
    fs::write(&secret, "peer private contents\n")?;
    let output = fixture
        .connection(
            "tenant",
            "fixture",
            &format!(
                "USE tenant; LOAD DATA INFILE '{}' INTO TABLE imported",
                secret.display()
            ),
        )
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("command denied"));
    assert_eq!(
        fixture
            .connect(
                "tenant",
                "fixture",
                "SELECT COUNT(*) AS n FROM tenant.imported"
            )?
            .trim(),
        "n\n0"
    );
    Ok(())
}

#[test]
fn trusted_file_remote_sync_remains_available_with_filesystem_sql_disabled() -> TestResult {
    let fixture = Fixture::new()?;
    let _server = fixture.start(DoltTransport::Tcp)?;
    let remote = fixture.root.path().join("canonical-remote");
    fixture.connect(
        "root",
        "",
        &format!(
            "CREATE DATABASE tenant; USE tenant; CREATE TABLE marker (id INT PRIMARY KEY); \
         INSERT INTO marker VALUES (42); CALL DOLT_ADD('.'); \
         CALL DOLT_COMMIT('-m', 'fixture', '--author', 'Fixture <fixture@example.invalid>'); \
         CALL DOLT_REMOTE('add', 'origin', 'file://{}'); CALL DOLT_PUSH('origin', 'main');",
            remote.display()
        ),
    )?;
    let restored = fixture.root.path().join("remote-reader");
    output(
        fixture
            .command("dolt")
            .args(["clone"])
            .arg(format!("file://{}", remote.display()))
            .arg(&restored),
    )?;
    assert_eq!(
        output(
            fixture
                .command("dolt")
                .arg("--data-dir")
                .arg(&restored)
                .args(["sql", "-r", "csv", "-q", "SELECT id FROM marker"])
        )?
        .trim(),
        "id\n42"
    );
    Ok(())
}

#[test]
fn server_configuration_preserves_dollar_and_control_characters_in_paths() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.data = fixture
        .root
        .path()
        .join("data ${UNSET_WRIX_CONFIG_VAR}\n\\\"");
    fixture.socket_dir = fixture.root.path().join("socket ${UNSET_WRIX_CONFIG_VAR}");
    fs::create_dir_all(&fixture.data)?;
    let _server = fixture.start(DoltTransport::UnixSocket)?;
    assert_eq!(fixture.connect("root", "", "SELECT 1 AS n")?.trim(), "n\n1");
    Ok(())
}

#[test]
fn failed_bootstrap_does_not_start_the_sql_server() -> TestResult {
    let fixture = Fixture::new()?;
    let invalid_data = fixture.root.path().join("not-a-directory");
    fs::write(&invalid_data, "preserve me")?;
    let result = fixture
        .command("sh")
        .args([
            "-c",
            &server_command(
                DoltTransport::Tcp,
                &invalid_data,
                Path::new(&fixture.socket_dir),
                fixture.port,
            ),
        ])
        .output()?;
    assert!(!result.status.success());
    assert!(!fixture.socket_dir.join("dolt.sock").exists());
    assert_eq!(fs::read_to_string(invalid_data)?, "preserve me");
    Ok(())
}
