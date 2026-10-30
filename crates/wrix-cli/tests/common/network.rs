//! Private native-fixture networks keep bind-then-launch port allocation independent of peers.

use super::TestResult;

#[cfg(target_os = "linux")]
const CHILD: &str = "WRIX_TEST_PRIVATE_NETWORK";

/// Re-execute only the current test in its own network; the caller's worker boundary is retained.
pub fn isolate() -> TestResult<bool> {
    #[cfg(target_os = "linux")]
    {
        let test = std::thread::current();
        let name = test.name().ok_or("native fixture test name missing")?;
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return Ok(false);
        }
        let output = command(name)?.output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    Ok(false)
}

#[cfg(target_os = "linux")]
fn command(name: &str) -> TestResult<std::process::Command> {
    let mut command = std::process::Command::new("timeout");
    command
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
            "bash",
            "-c",
            "set -euo pipefail; ip link set lo up; exec \"$@\"",
            "fixture",
        ])
        .arg(std::env::current_exe()?)
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name);
    Ok(command)
}

#[test]
#[cfg(target_os = "linux")]
fn private_network_keeps_parent_port_reservations_and_worker_boundary() -> TestResult {
    use std::{net::TcpListener, path::Path};
    const PORT: &str = "WRIX_TEST_PARENT_NETWORK_PORT";
    const WORKER: &str = "WRIX_TEST_PARENT_WORKER_MARKER";
    if let Ok(port) = std::env::var(PORT) {
        assert!(!isolate()?);
        let _listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>()?))?;
        assert_eq!(
            Path::new("/etc/wrix/image-agent").exists(),
            std::env::var(WORKER)? == "true"
        );
        return Ok(());
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let marker = Path::new("/etc/wrix/image-agent").exists();
    let test = std::thread::current();
    let output = command(test.name().ok_or("fixture test name missing")?)?
        .env(PORT, listener.local_addr()?.port().to_string())
        .env(WORKER, marker.to_string())
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(Path::new("/etc/wrix/image-agent").exists(), marker);
    assert!(TcpListener::bind(listener.local_addr()?).is_err());
    Ok(())
}
