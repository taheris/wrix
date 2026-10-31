//! Owned native controller; only the external user-manager/closure-pin commands are fixtures.

#[path = "native_roles.rs"]
mod native_roles;

#[path = "native_sql.rs"]
mod native_sql;

use super::TestResult;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use wrix_service::control::{Client, Operation, Reply, supervision::Package};

pub struct Server {
    pub(super) child: Child,
    pub(super) socket: PathBuf,
    calls: PathBuf,
    pub(super) manager: Option<PathBuf>,
    sql: native_sql::Server,
    roles: native_roles::Server,
    pub(super) backend_environment: PathBuf,
}

impl Server {
    pub fn start(root: &Path) -> TestResult<Self> {
        Self::start_at(root, &root.join("state/wrix"))
    }

    pub fn start_at(root: &Path, state: &Path) -> TestResult<Self> {
        let binding = std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
            .ok_or("missing native controller fixture binding")?;
        let package = Package::load(Path::new(&binding))?;
        let declared: serde_json::Value = serde_json::from_slice(&fs::read(&binding)?)?;
        let runtime = Path::new(
            declared["runtime"]
                .as_str()
                .ok_or("missing bound controller runtime")?,
        )
        .join("bin");
        let path = std::env::join_paths(std::iter::once(runtime.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("missing fixture PATH")?),
        ))?;
        let backend_environment = root.join("controller-backend-environment.json");
        let control = state.join("control");
        wrix_core::private_directory::prepare_with_parents(state)?;
        wrix_core::private_directory::prepare_with_parents(&control)?;
        let sql = native_sql::Server::start_at(root, &state.join("service-data/dolt"))?;
        let roles = native_roles::Server::start(root, &state.join("service-data/compiler"))?;
        let error = fs::File::create(root.join("controller.stderr"))?;
        let child = Command::new(package.executable())
            .arg(&control)
            .env(
                "WRIX_CONTAINER_RUNTIME",
                runtime.join(if cfg!(target_os = "macos") {
                    "container"
                } else {
                    "podman"
                }),
            )
            .env("WRIX_HOST_RUNTIME_PATH", &path)
            .env("PATH", path)
            .env("WRIX_TEST_BACKEND_ENV", &backend_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(error)
            .spawn()?;
        let mut server = Self {
            child,
            socket: control.join("authority/control.sock"),
            calls: root.join("controller-calls"),
            manager: None,
            sql,
            roles,
            backend_environment,
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

    #[cfg(target_os = "linux")]
    pub fn start_supervised(root: &Path) -> TestResult<Self> {
        Self::supervise(Self::start(root)?, root, &root.join("state/wrix"))
    }

    #[cfg(target_os = "linux")]
    pub fn supervise(mut server: Self, root: &Path, state: &Path) -> TestResult<Self> {
        use wrix_service::control::supervision::{Lifecycle, Native, Platform};
        let control = state.join("control");
        let manager = root.join("controller-supervisor");
        let script = root.join("controller-supervision-runtime");
        fs::write(
            &script,
            include_str!("../fixtures/controller-supervision.sh"),
        )?;
        fs::write(
            &manager,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec bash {} {} {} \"$@\"\n",
                shell_single_quote(script.to_str().ok_or("non-UTF-8 fixture script")?),
                shell_single_quote(control.to_str().ok_or("non-UTF-8 fixture control")?),
                server.child.id()
            ),
        )?;
        super::set_mode(&manager, 0o700)?;
        let pin = root.join("controller-pin");
        fs::write(
            &pin,
            "#!/usr/bin/env bash\nset -euo pipefail\n[[ $# == 5 && $1 == --realise && $2 == /nix/store/* && $3 == --add-root && $4 == */roots/* && $5 == --indirect ]]\nln -sfn -- \"$2\" \"$4\"\n",
        )?;
        super::set_mode(&pin, 0o700)?;
        let binding = std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
            .ok_or("missing native controller fixture binding")?;
        let package = Package::load(Path::new(&binding))?;
        Lifecycle::lock(&control)?.start(
            &Native::new(
                Platform::Linux,
                rustix::process::geteuid().as_raw(),
                manager.clone(),
                pin,
            ),
            &package,
        )?;
        server.manager = Some(manager);
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
            .env("WRIX_TEST_CONTROLLER_CALLS", &self.calls)
            .env("WRIX_TEST_SQL_CONTROL", &self.sql.socket)
            .env("WRIX_TEST_ROLES_CONTROL", &self.roles.socket)
            .env(
                "WRIX_TEST_SQL_CLIENT",
                std::env::var_os("WRIX_TEST_SQL_CLIENT")
                    .ok_or("missing native SQL wire client fixture")?,
            );
        if let Some(manager) = &self.manager {
            command.env("WRIX_TEST_CONTROLLER_MANAGER", manager);
        }
        let environment: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        let mut file = tempfile::NamedTempFile::new_in(
            self.backend_environment
                .parent()
                .ok_or("missing fixture environment parent")?,
        )?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec(&environment)?)?;
        file.persist(&self.backend_environment)?;
        Ok(())
    }

    pub fn stop(&mut self) -> io::Result<()> {
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
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(root) = self.backend_environment.parent()
            && let Ok(bytes) = fs::read(root.join("controller.stderr"))
        {
            tracing::error!(diagnostics = %String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(8192)..]), "owned controller failure diagnostics");
        }
        if let Err(error) = self.stop() {
            tracing::error!(%error, "owned controller fixture cleanup failed");
        }
    }
}

pub fn shell_single_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}
