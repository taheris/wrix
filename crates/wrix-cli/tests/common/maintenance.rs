//! Real controller upgrade; only the external user-manager and retention calls are adapted.
use super::TestResult;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use wrix_service::control::supervision::Package;

pub struct Maintenance {
    child: Child,
    pub control: PathBuf,
    pub adapter: PathBuf,
    pub package: Package,
    pub binding: PathBuf,
}

impl Maintenance {
    pub fn start(root: &Path) -> TestResult<Self> {
        // Seed real private authority, then emulate only the cached-v1 ownership
        // boundary. The upgraded daemon and authenticated IPC remain native.
        drop(super::controller::Server::start(root)?);
        let binding = PathBuf::from(
            std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
                .ok_or("missing controller fixture binding")?,
        );
        let package = Package::load(&binding)?;
        let control = root.join("state/wrix/control");
        let registry = control.join("authority/namespaces.json");
        let mut policy: serde_json::Value = serde_json::from_slice(&fs::read(&registry)?)?;
        policy["version"] = serde_json::json!(1);
        fs::write(&registry, serde_json::to_vec(&policy)?)?;
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/controller-maintenance.py");
        let child = Command::new("python3")
            .arg(&script)
            .arg("serve")
            .arg(&control)
            .arg(package.executable())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("maintenance.stderr"))?)
            .spawn()?;
        let adapter = root.join("controller-manager");
        fs::write(
            &adapter,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec python3 '{}' request '{}' \"$@\"\n",
                script.display(),
                control.join("manager.sock").display()
            ),
        )?;
        super::set_mode(&adapter, 0o700)?;
        let mut fixture = Self {
            child,
            control,
            adapter,
            package,
            binding,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !fixture.control.join("legacy-ready").exists() {
            if let Some(status) = fixture.child.try_wait()? {
                return Err(format!("maintenance adapter exited: {status}").into());
            }
            if std::time::Instant::now() >= deadline {
                return Err("legacy ownership fixture timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut previous: serde_json::Value = serde_json::from_slice(&fs::read(&fixture.binding)?)?;
        previous["controller"] = serde_json::json!("/nix/store/legacy-host-controller");
        let manifest = root.join("legacy-package.json");
        fs::write(&manifest, serde_json::to_vec(&previous)?)?;
        let pin = root.join("pin-controller");
        fs::write(&pin, "#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n")?;
        super::set_mode(&pin, 0o700)?;
        let lifecycle = wrix_service::control::supervision::Lifecycle::lock(&fixture.control)?;
        let native = wrix_service::control::supervision::Native::new(
            wrix_service::control::supervision::Platform::Linux,
            1000,
            fixture.adapter.clone(),
            pin,
        );
        lifecycle.start(&native, &Package::load(&manifest)?)?;
        Ok(fixture)
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env("WRIX_CONTROLLER_PACKAGE_BINDING", &self.binding)
            .env("WRIX_TEST_CONTROLLER_MANAGER", &self.adapter)
            .env(
                "WRIX_TEST_CONTROLLER_CALLS",
                self.control.join("retention.calls"),
            );
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        let result = (|| -> io::Result<()> {
            if self.child.try_wait()?.is_none() {
                let status = Command::new("kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .status()?;
                if !status.success() {
                    return Err(io::Error::other("could not stop owned maintenance adapter"));
                }
            }
            self.child.wait()?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::error!(%error, "owned maintenance fixture cleanup failed");
        }
    }
}
