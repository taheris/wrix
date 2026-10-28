//! Test-only observations run inside the separately bound service VM, never on the physical host store.
use super::*;
#[derive(Clone, Copy)]
pub(super) enum Observation {
    Boot,
    Serve,
    Snapshot,
    Stop,
    KillDaemon,
}

pub(super) fn command(
    runtime: &Command,
    host: &wrix_state::host::Host,
    selection: &Selection,
    observation: Observation,
) -> Command {
    let pool = Admission::acquire(selection, system())
        .unwrap()
        .pool()
        .clone();
    let root = host
        .roots()
        .state()
        .join("control/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str());
    let record = worker::pool_record(host, selection);
    assert_eq!(record["state"]["phase"], "serving");
    let port = record["state"]["connection"]["port"].as_u64().unwrap();
    let mut command = Command::new(
        Path::new(runtime.get_program())
            .parent()
            .unwrap()
            .join("ssh"),
    );
    command.env_clear().envs(
        runtime
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key, value))),
    );
    command
        .args([
            "-F",
            "/dev/null",
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ConnectTimeout=2",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
        ])
        .arg(format!(
            "UserKnownHostsFile=\"{}\"",
            root.join("known-hosts").display()
        ))
        .arg("-p")
        .arg(port.to_string())
        .arg("-i")
        .arg(root.join("admin"))
        .arg("root@127.0.0.1")
        .arg(match observation {
            Observation::Boot => "wrix-native-observe-boot-id",
            Observation::Serve => "wrix-native-observe-serve",
            Observation::Snapshot => "wrix-native-observe-snapshot",
            Observation::Stop => "wrix-native-observe-stop",
            Observation::KillDaemon => "wrix-native-observe-kill-daemon",
        });
    command
}
pub(super) fn kill_daemon(
    runtime: &Command,
    host: &wrix_state::host::Host,
    selection: &Selection,
) -> serde_json::Value {
    let output = command(runtime, host, selection, Observation::KillDaemon)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native daemon signal command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let proof: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("native daemon signal receipt; uncertain signaling is not a pass");
    assert!(proof["signaled_native_daemon"].as_u64().unwrap() > 1);
    assert_eq!(proof["signal"], 9);
    proof
}
pub(super) struct Probe {
    process: Running,
    snapshot: Command,
    stop: Command,
}
impl Probe {
    pub(super) fn start(
        runtime: &Command,
        host: &wrix_state::host::Host,
        selection: &Selection,
        control: &Path,
    ) -> Self {
        let log = control.join("service-native-control.json");
        let mut launch = command(runtime, host, selection, Observation::Serve);
        launch
            .stdout(fs::File::create(&log).unwrap())
            .stderr(fs::File::create(control.join("service-native-control.stderr")).unwrap());
        let mut process = Running(launch.spawn().unwrap());
        let deadline = Instant::now() + BUDGET;
        loop {
            let bytes = fs::read(&log).unwrap();
            if bytes.ends_with(b"\n") {
                let ready: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(ready["ready"], true);
                assert!(ready["pid"].as_u64().unwrap() > 1);
                break;
            }
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "native service control exited before readiness"
            );
            assert!(
                Instant::now() < deadline,
                "UNAVAILABLE: service-VM network positive controls"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Self {
            process,
            snapshot: command(runtime, host, selection, Observation::Snapshot),
            stop: command(runtime, host, selection, Observation::Stop),
        }
    }
    pub(super) fn snapshot(&mut self) -> serde_json::Value {
        assert!(
            self.process.0.try_wait().unwrap().is_none(),
            "known-live controls disappeared"
        );
        let output = self.snapshot.output().unwrap();
        assert!(
            output.status.success(),
            "native service observation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["outside_readable_unprivileged"], true);
        assert_eq!(value["known_live_ipv4"], true);
        assert_eq!(value["known_live_ipv6"], true);
        value
    }
    pub(super) fn stop(mut self) {
        let output = self.stop.output().unwrap();
        assert!(
            output.status.success(),
            "native control stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deadline = Instant::now() + BUDGET;
        loop {
            if let Some(status) = self.process.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native test control did not exit"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
pub(super) fn resources(before: &serde_json::Value, after: &serde_json::Value) {
    assert_eq!(
        before["limits"], after["limits"],
        "worker changed service-owned resource ceilings"
    );
    assert!(
        after["registered_paths"].as_u64().unwrap() > before["registered_paths"].as_u64().unwrap(),
        "cold substitution/build never registered actual service paths"
    );
    assert!(
        after["registered_nar_bytes"].as_u64().unwrap()
            > before["registered_nar_bytes"].as_u64().unwrap()
    );
    assert!(
        after["events"]["memory.events"]["oom_kill"]
            .as_u64()
            .unwrap()
            > before["events"]["memory.events"]["oom_kill"]
                .as_u64()
                .unwrap(),
        "memory exhaustion was not observed by the actual cgroup"
    );
    assert!(
        after["events"]["work/pids.events"]["max"].as_u64().unwrap()
            > before["events"]["work/pids.events"]["max"]
                .as_u64()
                .unwrap(),
        "PID limit was not exercised"
    );
}
