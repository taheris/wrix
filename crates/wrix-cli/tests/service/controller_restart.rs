//! Drive the real controller child when the external user-manager fixture requests a restart.
use super::super::{TestResult, controller};
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use wrix_service::control::supervision::Package;

impl controller::Server {
    pub(super) fn enable_restart(&self) -> TestResult {
        let root = self
            .backend_environment
            .parent()
            .ok_or("missing fixture root")?;
        let control = self
            .socket
            .parent()
            .and_then(Path::parent)
            .ok_or("missing control root")?;
        let pidfile = root.join("controller.pid");
        fs::write(&pidfile, format!("{}\n", self.child.id()))?;
        let script = root.join("controller-resume-supervision");
        fs::write(
            &script,
            include_str!("../fixtures/controller-resume-supervision.sh"),
        )?;
        let manager = self.manager.as_ref().ok_or("missing fixture manager")?;
        fs::write(
            manager,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec bash {} {} {} \"$@\"\n",
                controller::shell_single_quote(script.to_str().ok_or("script path")?),
                controller::shell_single_quote(control.to_str().ok_or("control path")?),
                controller::shell_single_quote(pidfile.to_str().ok_or("pid path")?)
            ),
        )?;
        fs::write(
            root.join("controller-pin"),
            "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$#\" == 5 && \"$1\" == --realise && \"$2\" == /nix/store/* && \"$3\" == --add-root && ( \"$4\" == */roots/* || \"$4\" == */maintenance-roots/* ) && \"$5\" == --indirect ]]\nln -sfn -- \"$2\" \"$4\"\n",
        )?;
        Ok(())
    }

    pub(super) fn restart_when_requested(&mut self, update: &mut Child) -> TestResult {
        let control = self
            .socket
            .parent()
            .and_then(Path::parent)
            .ok_or("missing control root")?;
        let deadline = Instant::now() + Duration::from_secs(25);
        while !control.join("restart.request").exists() {
            if let Some(status) = update.try_wait()? {
                return Err(format!("update exited before controller restart: {status}").into());
            }
            if Instant::now() >= deadline {
                return Err("fixture restart request deadline".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.child.wait()?;
        let package = Package::load(&control.join("package.json"))?;
        let declared = serde_json::to_value(&package)?;
        let runtime = Path::new(declared["runtime"].as_str().ok_or("missing runtime")?).join("bin");
        let path = std::env::join_paths(std::iter::once(runtime).chain(std::env::split_paths(
            &std::env::var_os("PATH").ok_or("missing PATH")?,
        )))?;
        let root = self
            .backend_environment
            .parent()
            .ok_or("missing fixture root")?;
        self.child = Command::new(package.executable())
            .arg(control)
            .env("PATH", path)
            .env("WRIX_TEST_BACKEND_ENV", &self.backend_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("controller.stderr"))?,
            )
            .spawn()?;
        fs::write(
            root.join("controller.pid"),
            format!("{}\n", self.child.id()),
        )?;
        fs::remove_file(control.join("restart.request"))?;
        Ok(())
    }
}
