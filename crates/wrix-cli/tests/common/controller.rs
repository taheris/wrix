//! Owned native controller; only the external user-manager/closure-pin commands are fixtures.

use super::TestResult;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use wrix_service::control::{Client, Operation, Reply, supervision::Package};

pub struct Server {
    child: Child,
    socket: PathBuf,
    calls: PathBuf,
}

impl Server {
    pub fn start(root: &Path) -> TestResult<Self> {
        let binding = std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
            .ok_or("missing native controller fixture binding")?;
        let package = Package::load(Path::new(&binding))?;
        let control = root.join("state/wrix/control");
        wrix_core::private_directory::prepare_with_parents(&control)?;
        let error = fs::File::create(root.join("controller.stderr"))?;
        let child = Command::new(package.executable())
            .arg(&control)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(error)
            .spawn()?;
        let mut server = Self {
            child,
            socket: control.join("authority/control.sock"),
            calls: root.join("controller-calls"),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(status) = server.child.try_wait()? {
                        return Err(io::Error::other(format!(
                            "native controller exited: {status}"
                        )));
                    }
                    match Client::open(&control.join("authority")) {
                        Ok(client) => match client.request(Operation::Inspect {}).await {
                            Ok(Reply::Snapshot { version: 2, .. }) => return Ok(()),
                            Ok(_) => {
                                return Err(io::Error::other(
                                    "incompatible native controller fixture",
                                ));
                            }
                            Err(wrix_service::control::Error::Io(error))
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                                ) =>
                            {
                                tracing::debug!(%error, "awaiting fixture controller readiness");
                            }
                            Err(error) => return Err(io::Error::other(error)),
                        },
                        Err(wrix_service::control::Error::Io(error))
                            if error.kind() == io::ErrorKind::NotFound =>
                        {
                            tracing::debug!(%error, "awaiting fixture controller authority");
                        }
                        Err(error) => return Err(io::Error::other(error)),
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(io::Error::other)?
        })?;
        Ok(server)
    }

    pub fn configure(&self, command: &mut Command) -> TestResult {
        command
            .env(
                "WRIX_CONTROLLER_PACKAGE_BINDING",
                std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
                    .ok_or("missing native controller binding")?,
            )
            .env("WRIX_TEST_CONTROLLER_SOCKET", &self.socket)
            .env("WRIX_TEST_CONTROLLER_CALLS", &self.calls);
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let stopped = (|| -> io::Result<()> {
            if self.child.try_wait()?.is_none() {
                let status = Command::new("kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .status()?;
                if !status.success() {
                    return Err(io::Error::other("could not stop owned fixture controller"));
                }
            }
            self.child.wait()?;
            Ok(())
        })();
        if let Err(error) = stopped {
            tracing::error!(%error, "owned controller fixture cleanup failed");
        }
    }
}
