//! Test-only observations run inside the separately bound service VM, never on the physical host store.
use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    python: PathBuf,
    probe: PathBuf,
}
impl Inputs {
    fn load(binding: &wrix_core::nix_store::binding::Binding) -> Self {
        let inputs: Self = serde_json::from_slice(
            &fs::read(std::env::var_os("WRIX_TEST_POOL_NATIVE_INPUTS").unwrap()).unwrap(),
        )
        .unwrap();
        let closure = fs::read_to_string(binding.metadata().seed_closure.as_path()).unwrap();
        for path in [&inputs.python, &inputs.probe] {
            let root = path
                .ancestors()
                .find(|path| path.parent() == Some(Path::new("/nix/store")))
                .expect("immutable guest fixture path");
            assert!(
                closure.lines().any(|line| Path::new(line) == root),
                "native observation executable is absent from the actual seeded image: {path:?}"
            );
        }
        inputs
    }
    fn command(&self, runtime: &Command, owner: &str, operation: &str) -> Command {
        let mut command = worker::recipe(runtime);
        command
            .arg("exec")
            .arg(owner)
            .arg(&self.python)
            .arg(&self.probe)
            .arg(operation);
        command
    }
}
pub(super) fn kill_daemon(
    runtime: &Command,
    owner: &str,
    binding: &wrix_core::nix_store::binding::Binding,
) -> serde_json::Value {
    let inputs = Inputs::load(binding);
    let output = inputs
        .command(runtime, owner, "kill-daemon")
        .output()
        .unwrap();
    let proof: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("native daemon signal receipt; uncertain signaling is not a pass");
    assert!(proof["signaled_native_daemon"].as_u64().unwrap() > 1);
    assert_eq!(proof["signal"], 9);
    proof
}
pub(super) struct Probe {
    process: Running,
    inputs: Inputs,
    runtime: Command,
    owner: String,
}
impl Probe {
    pub(super) fn start(
        runtime: &Command,
        owner: &str,
        binding: &wrix_core::nix_store::binding::Binding,
        control: &Path,
    ) -> Self {
        let inputs = Inputs::load(binding);
        let log = control.join("service-native-control.json");
        let mut command = inputs.command(runtime, owner, "serve");
        command
            .stdout(fs::File::create(&log).unwrap())
            .stderr(fs::File::create(control.join("service-native-control.stderr")).unwrap());
        let mut process = Running(command.spawn().unwrap());
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
            inputs,
            runtime: worker::recipe(runtime),
            owner: owner.to_owned(),
        }
    }
    pub(super) fn snapshot(&mut self) -> serde_json::Value {
        assert!(
            self.process.0.try_wait().unwrap().is_none(),
            "known-live controls disappeared"
        );
        let output = self
            .inputs
            .command(&self.runtime, &self.owner, "snapshot")
            .output()
            .unwrap();
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
        let output = self
            .inputs
            .command(&self.runtime, &self.owner, "stop")
            .output()
            .unwrap();
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
