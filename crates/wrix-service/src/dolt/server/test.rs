use super::*;
use std::{fs, net::TcpListener, os::unix::fs::PermissionsExt, time::Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn native_configuration_never_exposes_its_socket_in_the_worker_alias_directory() {
    let root = Path::new("/service-data/dolt");
    for tcp in [false, true] {
        let config = configuration_value(root, tcp);
        assert_eq!(config["listener"]["host"], "127.0.0.1");
        assert_eq!(config["listener"]["port"], 13306);
        assert_eq!(config["listener"]["socket"], "/run/wrix/dolt/native.sock");
        assert_eq!(config["system_variables"]["secure_file_priv"], "/dev/null");
    }
}

#[tokio::test]
async fn contended_original_owner_does_not_clear_the_previous_release_certificate() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    let owner = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.path().join("owner.lock"))?;
    owner.lock_exclusive()?;
    let certificate = root.path().join("native-release.json");
    fs::write(&certificate, b"retained original certificate")?;
    let result = run(root.path(), false).await;
    assert!(
        matches!(&result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock),
        "contended original ownership returned {result:?}"
    );
    assert_eq!(fs::read(certificate)?, b"retained original certificate");
    assert!(!root.path().join("databases").exists());
    Ok(())
}

pub(super) fn native_query(
    data: &Path,
    port: u16,
    query: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("dolt")
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--no-tls",
            "--user",
            "wrixadmin",
            "--use-db",
            "controller",
            "sql",
            "-r",
            "json",
            "-q",
            query,
        ])
        .env(
            "DOLT_CLI_PASSWORD",
            fs::read_to_string(data.join("admin.password"))?,
        )
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .current_dir(data)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values.into_iter().next().unwrap_or(serde_json::Value::Null))
}

pub(super) async fn start_native(
    data: &Path,
    config: &Path,
    socket: &Path,
) -> Result<tokio::process::Child, Box<dyn std::error::Error>> {
    let log = File::create(data.join("native.log"))?;
    let mut child = tokio::process::Command::new("dolt")
        .args(["sql-server", "--config"])
        .arg(config)
        .current_dir(data)
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .env_remove("DOLT_CLI_PASSWORD")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket.exists() {
        assert!(
            child.try_wait()?.is_none() && Instant::now() < deadline,
            "{}",
            fs::read_to_string(data.join("native.log"))?
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(child)
}

#[tokio::test]
async fn native_dolt_term_reaps_zero_before_original_flock_release_and_data_reopen() -> TestResult {
    let root = tempfile::Builder::new().prefix("wd-").tempdir()?;
    let data = root.path();
    fs::create_dir(data.join("databases"))?;
    let owner = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(data.join("owner.lock"))?;
    owner.lock_exclusive()?;
    let original = owner.metadata()?;
    initialize(data)?;
    let password = fs::read(data.join("admin.password"))?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let socket = data.join("native.sock");
    let mut configuration = configuration_value(data, false);
    configuration["listener"]["port"] = port.into();
    configuration["listener"]["socket"] = socket.to_str().unwrap().into();
    let config = data.join("native.json");
    fs::write(&config, serde_json::to_vec(&configuration)?)?;
    let mut child = start_native(data, &config, &socket).await?;
    assert_eq!(
        native_query(data, port, "SELECT @@GLOBAL.secure_file_priv AS hardened")?["rows"][0]["hardened"],
        "/dev/null"
    );
    native_query(
        data,
        port,
        "CREATE TABLE retained(id INT PRIMARY KEY); INSERT INTO retained VALUES(7)",
    )?;
    let contender = File::options()
        .read(true)
        .write(true)
        .open(data.join("owner.lock"))?;
    assert!(contender.try_lock_exclusive().is_err());
    stop(&mut child).await?;
    assert!(child.wait().await?.success());
    assert!(contender.try_lock_exclusive().is_err());
    drop(owner);
    contender.try_lock_exclusive()?;
    assert_eq!(contender.metadata()?.ino(), original.ino());
    assert_eq!(contender.metadata()?.dev(), original.dev());
    socket::clear_path(data, &socket)?;
    initialize(data)?;
    assert_eq!(fs::read(data.join("admin.password"))?, password);
    let mut reopened = start_native(data, &config, &socket).await?;
    assert_eq!(
        native_query(data, port, "SELECT id FROM retained")?["rows"][0]["id"],
        "7"
    );
    stop(&mut reopened).await?;
    assert!(reopened.wait().await?.success());
    Ok(())
}
