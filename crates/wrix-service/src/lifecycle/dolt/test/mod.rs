use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{DoltTransport, probe_tcp, server_command};

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
        output(self.command("dolt").args([
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
        ]))
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
fn sql_probe_times_out_when_a_tcp_listener_never_speaks_mysql() -> TestResult {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let start = Instant::now();
    let error = probe_tcp(listener.local_addr()?.port(), Duration::from_millis(100)).unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
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
