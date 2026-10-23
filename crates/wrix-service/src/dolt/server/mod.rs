//! The service-image entry point cohosts Dolt, Celler and the bounded sync adapter.

use super::Error;
use fs2::FileExt;
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::Stdio,
    time::Duration,
};

pub(crate) mod socket;

/// The root credential remains inside the narrow service data mount, never a tenant export.
pub async fn run(data: &Path, tcp: bool) -> Result<(), Error> {
    super::private_directory(data)?;
    let ownership = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(data.join("owner.lock"))?;
    ownership.try_lock_exclusive()?;
    super::private_directory(&data.join("databases"))?;
    super::private_directory(&data.join("run"))?;
    socket::clear(data)?;
    initialize(data)?;
    let configuration = data.join("server.yaml");
    super::atomic_json(&configuration, &configuration_value(data, tcp))?;
    let mut dolt = tokio::process::Command::new("dolt")
        .args(["sql-server", "--config"])
        .arg(&configuration)
        .current_dir(data)
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .env_remove("DOLT_CLI_PASSWORD")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut celler = tokio::process::Command::new("sh")
        .args(["-c", wrix_cache::backend::layout::runtime::COMMAND])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let result = tokio::select! {
        result = dolt.wait() => { result?; Err(Error::Incomplete) },
        result = celler.wait() => { result?; Err(Error::Incomplete) },
        result = super::broker::serve(data, tcp) => result,
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    // Explicit container stop is intentionally disruptive; children get native TERM before the deadline.
    for child in [&mut dolt, &mut celler] {
        if let Some(id) = child.id() {
            let output = tokio::process::Command::new("kill")
                .args(["-TERM", &id.to_string()])
                .output()
                .await?;
            if !output.status.success() && child.try_wait()?.is_none() {
                return Err(Error::Incomplete);
            }
            tokio::time::timeout(Duration::from_secs(20), child.wait())
                .await
                .map_err(|_| Error::Incomplete)??;
        }
    }
    result
}

pub(crate) fn configuration_value(data: &Path, tcp: bool) -> serde_json::Value {
    serde_json::json!({
        "data_dir": data.join("databases").display().to_string().replace('$', "$$"),
        "cfg_dir": data.join("databases/.doltcfg").display().to_string().replace('$', "$$"),
        "privilege_file": data.join("databases/.doltcfg/privileges.db").display().to_string().replace('$', "$$"),
        "listener": {
            "host": if tcp { "0.0.0.0" } else { "127.0.0.1" },
            "port": 3306,
            "socket": data.join("run/dolt.sock").display().to_string().replace('$', "$$"),
        },
        "system_variables": { "secure_file_priv": "/dev/null" },
    })
}

fn initialize(data: &Path) -> Result<(), Error> {
    let path = data.join("admin.password");
    let password = match File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            let password = super::random_password()?;
            file.write_all(password.as_bytes())?;
            file.sync_all()?;
            password
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut password = String::new();
            File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?
                .read_to_string(&mut password)?;
            password
        }
        Err(error) => return Err(error.into()),
    };
    if password.len() != 64 || !password.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Incomplete);
    }
    if data.join("initialized").exists() {
        return Ok(());
    }
    let query = format!(
        "CREATE DATABASE IF NOT EXISTS controller; CREATE USER IF NOT EXISTS 'wrixadmin'@'localhost' IDENTIFIED BY {}; GRANT ALL PRIVILEGES ON *.* TO 'wrixadmin'@'localhost' WITH GRANT OPTION; DROP USER IF EXISTS 'root'@'localhost'",
        super::literal(&password)
    );
    let mut child = std::process::Command::new("dolt")
        .arg("--data-dir")
        .arg(data.join("databases"))
        .args(["sql"])
        .current_dir(data)
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .env_remove("DOLT_CLI_PASSWORD")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or(Error::Result)?
        .write_all(query.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::Native {
            stderr: String::from_utf8_lossy(&output.stderr).replace(&password, "[REDACTED]"),
        });
    }
    File::create(data.join("initialized"))?.sync_all()?;
    File::open(data)?.sync_all()?;
    Ok(())
}
