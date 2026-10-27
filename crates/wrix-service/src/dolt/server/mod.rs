//! The service-image entry point cohosts Dolt, Celler and the bounded sync adapter.

use super::Error;
use fs2::FileExt;
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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
    let metadata = ownership.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != std::fs::metadata(data)?.uid()
    {
        return Err(Error::UnsafePath);
    }
    ownership.try_lock_exclusive()?;
    crate::lifecycle::compiler::clear_backend_release(data)?;
    super::private_directory(Path::new("/run/wrix/dolt"))?;
    socket::clear_path(data, Path::new("/run/wrix/dolt/native.sock"))?;
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
        .env("WRIX_CELLER_DRAIN_SOCKET", "/run/wrix/celler/drain.sock")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let gate = crate::lifecycle::sql::Gate::default();
    let result = {
        let broker = super::broker::serve(data, tcp, gate.clone());
        let sql = crate::lifecycle::sql::serve(data, gate.clone());
        tokio::pin!(broker, sql);
        tokio::select! {
            result = dolt.wait() => { result?; Err(Error::Incomplete) },
            result = celler.wait() => {
                let status = result?;
                if crate::lifecycle::compiler::planned()? && status.success() {
                    let release = planned_release(&mut dolt);
                    tokio::pin!(release);
                    tokio::select! {
                        result = &mut release => {
                            result?;
                            drop(ownership);
                            return Ok(());
                        }
                        result = &mut broker => result.and(Err(Error::Incomplete)),
                        result = &mut sql => result.map_err(Error::from).and(Err(Error::Incomplete)),
                    }
                } else {
                    Err(Error::Native { stderr: format!("supervised Celler exited unexpectedly: {status}") })
                }
            },
            result = &mut broker => result.and(Err(Error::Incomplete)),
            result = &mut sql => result.map_err(Error::from).and(Err(Error::Incomplete)),
            _ = terminate.recv() => stopped(),
            _ = interrupt.recv() => stopped(),
        }
    };
    // Explicit host stop and unexpected failure retain the disruptive shutdown contract.
    for child in [&mut dolt, &mut celler] {
        tokio::time::timeout(Duration::from_secs(20), stop(child))
            .await
            .map_err(|_| Error::Incomplete)??;
    }
    result
}

async fn planned_release(dolt: &mut tokio::process::Child) -> Result<(), Error> {
    while !crate::lifecycle::compiler::released()? {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    loop {
        let response = tokio::task::spawn_blocking(|| {
            crate::lifecycle::sql::dispatch(crate::lifecycle::drain::Command::Quiesce)
        })
        .await
        .map_err(|_| Error::Task)?;
        match response {
            Ok(response) if response.accepted && response.status.drained() => break,
            Ok(response) => {
                tracing::warn!(status = ?response.status, "retaining Dolt owner until native SQL work finishes");
            }
            Err(error) => {
                tracing::warn!(%error, "retaining Dolt owner while SQL completion is uncertain");
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop(dolt).await
}

fn stopped() -> Result<(), Error> {
    if crate::lifecycle::compiler::planned()? {
        return Err(Error::Incomplete);
    }
    Ok(())
}

/// Planned backend release has no deadline or forced-kill escalation.
async fn stop(child: &mut tokio::process::Child) -> Result<(), Error> {
    if let Some(id) = child.id() {
        let output = tokio::process::Command::new("kill")
            .args(["-TERM", &id.to_string()])
            .output()
            .await?;
        if !output.status.success() && child.try_wait()?.is_none() {
            return Err(Error::Incomplete);
        }
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(Error::Native {
            stderr: format!("native backend did not close cleanly: {status}"),
        });
    }
    Ok(())
}

pub(crate) fn configuration_value(data: &Path, _tcp: bool) -> serde_json::Value {
    serde_json::json!({
        "data_dir": data.join("databases").display().to_string().replace('$', "$$"),
        "cfg_dir": data.join("databases/.doltcfg").display().to_string().replace('$', "$$"),
        "privilege_file": data.join("databases/.doltcfg/privileges.db").display().to_string().replace('$', "$$"),
        "listener": {
            "host": "127.0.0.1",
            "port": 13306,
            "socket": "/run/wrix/dolt/native.sock",
        },
        "system_variables": { "secure_file_priv": "/dev/null" },
    })
}

pub(crate) fn initialize(data: &Path) -> Result<(), Error> {
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

#[cfg(test)]
mod native_test;
#[cfg(test)]
mod test;
