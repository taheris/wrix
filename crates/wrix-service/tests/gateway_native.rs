//! The real service child and its private readiness/drain command boundary.

use std::{
    io::Write,
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    time::{Instant, timeout},
};
use wrix_service::gateway::{
    Secret,
    management::{Client, Status},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => (),
            Ok(None) => {
                if let Err(error) = self.0.kill() {
                    tracing::error!(%error, "native gateway cleanup kill failed");
                }
                if let Err(error) = self.0.wait() {
                    tracing::error!(%error, "native gateway cleanup wait failed");
                }
            }
            Err(error) => tracing::error!(%error, "native gateway cleanup inspection failed"),
        }
    }
}

#[tokio::test]
async fn native_binary_readiness_and_drain_require_the_generation_key_and_live_data_plane() {
    let data = TcpListener::bind("127.0.0.1:0").unwrap();
    let data_address = data.local_addr().unwrap();
    let management = TcpListener::bind("127.0.0.1:0").unwrap();
    let private = management.local_addr().unwrap();
    let key = Secret::generate().unwrap();
    let launch = serde_json::to_vec(&serde_json::json!({"version":1,"data":data_address,"management":private,"key":key.expose(),"protected":[data_address,private],"capacity":16})).unwrap();
    drop(data);
    drop(management);
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_wrix-egress-gateway"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    child.0.stdin.take().unwrap().write_all(&launch).unwrap();
    let client = Client::new(private, key.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(child.0.try_wait().unwrap().is_none());
        if let Ok(Ok(_)) = timeout(Duration::from_millis(100), client.status()).await {
            break;
        }
        assert!(Instant::now() < deadline, "native gateway did not start");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    client.verify(data_address).await.unwrap();
    assert!(
        Client::new(private, Secret::generate().unwrap())
            .status()
            .await
            .is_err()
    );
    for command in ["ready", "status", "drain"] {
        let mut probe = tokio::process::Command::new(env!("CARGO_BIN_EXE_wrix-egress-gateway"))
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        probe
            .stdin
            .take()
            .unwrap()
            .write_all(&launch)
            .await
            .unwrap();
        let output = timeout(Duration::from_secs(5), probe.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Status = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status.version, 1);
        assert_eq!(status.draining, command == "drain");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("\"key\""));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(key.expose()));
    }
    assert!(client.verify(data_address).await.is_err());
    assert!(child.0.try_wait().unwrap().is_none());
}

#[test]
fn native_binary_rejects_unknown_private_commands_before_reading_bootstrap() {
    let output = Command::new(env!("CARGO_BIN_EXE_wrix-egress-gateway"))
        .arg("forward-host")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
