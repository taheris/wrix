//! Real private supervisor/client subprocesses behind a mocked OCI boundary; not native runtime proof.

use crate::common::TestResult;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct Server {
    child: Child,
    root: PathBuf,
}

impl Server {
    pub fn start(root: &Path, namespace: &str) -> TestResult<Self> {
        fs::create_dir_all(root)?;
        let first = format!(
            "echo $$ > '{}'; exec sleep 60",
            root.join("celler.pid").display()
        );
        let second = format!(
            "echo $$ > '{}'; echo started >> '{}'; exec sleep 60",
            root.join("dolt.pid").display(),
            root.join("launches").display()
        );
        let log = File::create(root.join("supervisor.log"))?;
        let child = Command::new(env!("CARGO_BIN_EXE_wrix-service-supervisor"))
            .arg("serve")
            .arg(root)
            .arg(namespace)
            .arg(root.join("dolt.sock"))
            .args([first, second])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut server = Self {
            child,
            root: root.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root.join("dolt.pid").exists() || !root.join("celler.pid").exists() {
            if let Some(status) = server.child.try_wait()? {
                return Err(format!(
                    "component producer exited {status}: {}",
                    fs::read_to_string(root.join("supervisor.log"))?
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err("component producer did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(server)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn stop(&mut self) -> TestResult {
        if self.child.try_wait()?.is_none() {
            let status = Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()?;
            if !status.success() {
                return Err("cannot stop owned component producer".into());
            }
            self.child.wait()?;
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::warn!(%error, "owned component producer cleanup failed");
        }
    }
}
