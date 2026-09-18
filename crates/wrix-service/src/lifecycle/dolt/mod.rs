use std::{
    io,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use super::DoltTransport;

// Root is scoped to this workspace's service; TCP publication remains host-loopback-only.
const BOOTSTRAP_SQL: &str = "CREATE USER IF NOT EXISTS 'root'@'%'; \
    GRANT ALL PRIVILEGES ON *.* TO 'root'@'%' WITH GRANT OPTION;";

pub(super) fn server_command(
    transport: DoltTransport,
    data_dir: &Path,
    socket_dir: &Path,
    port: u16,
) -> String {
    let data = shell_quote(data_dir);
    let socket = shell_quote(&socket_dir.join("dolt.sock"));
    let directory = shell_quote(socket_dir);
    let bootstrap = match transport {
        DoltTransport::UnixSocket => String::new(),
        DoltTransport::Tcp => {
            format!("dolt --data-dir {data} sql -q \"{BOOTSTRAP_SQL}\" && ")
        }
    };
    let host = match transport {
        DoltTransport::UnixSocket => "127.0.0.1",
        DoltTransport::Tcp => "0.0.0.0",
    };
    format!(
        "mkdir -p {directory} && {bootstrap}exec dolt sql-server \
         --data-dir {data} --host {host} --port {port} --socket {socket}"
    )
}

pub(super) fn probe_tcp(port: u16, budget: Duration) -> io::Result<()> {
    if budget.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Dolt SQL probe timed out",
        ));
    }
    let timeout = budget.min(Duration::from_secs(1)).as_secs_f64().to_string();
    let output = Command::new("timeout")
        .args(["--signal=KILL", &timeout, "dolt"])
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["--no-tls", "--user", "root", "--password", ""])
        .args(["sql", "-q", "SELECT 1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(if stderr.trim().is_empty() {
        format!(
            "Dolt SQL authentication probe failed or timed out ({})",
            output.status
        )
    } else {
        format!("Dolt SQL authentication probe failed: {}", stderr.trim())
    }))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod test;
