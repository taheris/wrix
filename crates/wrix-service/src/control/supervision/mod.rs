//! Caller-independent user supervision; only host package metadata selects executable closures.

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error as ThisError;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// host controller lifecycle I/O failed: {0}
    Io(#[from] io::Error),
    /// invalid host controller package metadata: {0}
    Encoding(#[from] serde_json::Error),
    /// host controller package paths must be absolute normalized Nix store paths
    Package,
    /// user supervision is unavailable; configure a systemd user manager or a launchd GUI session: {0}
    Supervisor(String),
    /// controller closure retention failed: {0}
    Root(String),
    /// controller lifecycle directory must be canonical and private
    PrivateRoot,
    /// this platform has no supported host-user supervisor
    Unsupported,
}

/// Immutable host-package association, independent of the service image's Linux platform.
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct Package {
    controller: PathBuf,
    runtime: PathBuf,
    image: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    controller: PathBuf,
    runtime: PathBuf,
    image: PathBuf,
}

impl Package {
    /// Read the immutable binding set by the host package wrapper, not project configuration.
    pub fn from_env() -> Result<Self, Error> {
        let path = std::env::var_os("WRIX_CONTROLLER_PACKAGE_BINDING").ok_or(Error::Package)?;
        Self::load(Path::new(&path))
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let wire: Wire = serde_json::from_slice(&fs::read(path)?)?;
        let package = Self {
            controller: wire.controller,
            runtime: wire.runtime,
            image: wire.image,
        };
        for path in [&package.controller, &package.runtime, &package.image] {
            if !path.starts_with("/nix/store")
                || path.components().count() != 4
                || path.components().any(|part| {
                    !matches!(
                        part,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
            {
                return Err(Error::Package);
            }
        }
        if package
            .image
            .extension()
            .is_none_or(|extension| extension != "drv")
        {
            return Err(Error::Package);
        }
        Ok(package)
    }

    pub fn executable(&self) -> PathBuf {
        self.controller.join("bin/wrix-host-controller")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Platform {
    Linux,
    Darwin,
}

impl Platform {
    pub const fn current() -> Result<Self, Error> {
        if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Darwin)
        } else {
            Err(Error::Unsupported)
        }
    }
}

/// Native command adapter. There is deliberately no direct-spawn fallback.
pub struct Native {
    platform: Platform,
    supervisor: PathBuf,
    nix_store: PathBuf,
    uid: u32,
}

impl Native {
    pub fn new(platform: Platform, uid: u32) -> Self {
        Self {
            platform,
            uid,
            supervisor: match platform {
                Platform::Linux => "systemctl",
                Platform::Darwin => "/bin/launchctl",
            }
            .into(),
            nix_store: "nix-store".into(),
        }
    }

    pub fn for_package(platform: Platform, uid: u32, package: &Package) -> Self {
        Self {
            platform,
            uid,
            supervisor: match platform {
                Platform::Linux => package.runtime.join("bin/systemctl"),
                Platform::Darwin => "/bin/launchctl".into(),
            },
            nix_store: package.runtime.join("bin/nix-store"),
        }
    }

    fn command(&self, args: &[&std::ffi::OsStr]) -> Result<(), Error> {
        let output = Command::new(&self.supervisor)
            .args(args)
            .output()
            .map_err(|error| Error::Supervisor(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::Supervisor(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    fn check(&self) -> Result<(), Error> {
        match self.platform {
            Platform::Linux => self.command(&["--user".as_ref(), "show-environment".as_ref()]),
            Platform::Darwin => {
                self.command(&["print".as_ref(), format!("gui/{}", self.uid).as_ref()])
            }
        }
    }

    fn pin(&self, root: &Path, package: &Package) -> Result<(), Error> {
        fs::create_dir_all(root)?;
        for (name, closure) in [
            ("controller", &package.controller),
            ("runtime", &package.runtime),
            ("image", &package.image),
        ] {
            let output = Command::new(&self.nix_store)
                .arg("--realise")
                .arg(closure)
                .arg("--add-root")
                .arg(root.join(name))
                .arg("--indirect")
                .output()?;
            if !output.status.success() {
                return Err(Error::Root(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
        }
        Ok(())
    }

    fn start(&self, root: &Path, package: &Package) -> Result<(), Error> {
        let executable = package.executable();
        match self.platform {
            Platform::Linux => {
                let unit = root.join("wrix-host-controller.service");
                let contents = format!(
                    "[Unit]\nDescription=Wrix host controller\n[Service]\nType=exec\nExecStart={} {}\nRestart=on-failure\nRestartSec=1\n",
                    systemd_path(&executable),
                    systemd_path(root)
                );
                fs::write(&unit, contents)?;
                self.command(&["--user".as_ref(), "link".as_ref(), unit.as_os_str()])?;
                self.command(&["--user".as_ref(), "daemon-reload".as_ref()])?;
                self.command(&[
                    "--user".as_ref(),
                    "start".as_ref(),
                    "wrix-host-controller.service".as_ref(),
                ])
            }
            Platform::Darwin => {
                let plist = root.join("org.wrix.host-controller.plist");
                let contents = format!(
                    "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>org.wrix.host-controller</string><key>ProgramArguments</key><array><string>{}</string><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict></dict></plist>",
                    xml(&executable)?,
                    xml(root)?
                );
                fs::write(&plist, contents)?;
                let service = format!("gui/{}/org.wrix.host-controller", self.uid);
                let loaded = Command::new(&self.supervisor)
                    .args(["print", &service])
                    .output()?;
                if loaded.status.success() {
                    self.command(&["kickstart".as_ref(), service.as_ref()])
                } else {
                    self.command(&[
                        "bootstrap".as_ref(),
                        format!("gui/{}", self.uid).as_ref(),
                        plist.as_os_str(),
                    ])
                }
            }
        }
    }

    fn stop(&self) -> Result<(), Error> {
        match self.platform {
            Platform::Linux => self.command(&[
                "--user".as_ref(),
                "stop".as_ref(),
                "wrix-host-controller.service".as_ref(),
            ]),
            Platform::Darwin => self.command(&[
                "bootout".as_ref(),
                format!("gui/{}/org.wrix.host-controller", self.uid).as_ref(),
            ]),
        }
    }
}

fn systemd_path(path: &Path) -> String {
    let escaped: String = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| {
            if *byte == b'%' {
                "%%".to_owned()
            } else {
                format!("\\x{byte:02x}")
            }
        })
        .collect();
    format!("\"{escaped}\"")
}

fn xml(path: &Path) -> Result<String, Error> {
    Ok(path
        .to_str()
        .ok_or(Error::PrivateRoot)?
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

/// A serialized lifecycle transaction; its lock is never inherited by the controller.
pub struct Lifecycle {
    root: PathBuf,
    _lock: File,
}

impl Lifecycle {
    pub fn lock(root: &Path) -> Result<Self, Error> {
        use std::os::unix::fs::PermissionsExt;
        if root
            .parent()
            .ok_or(Error::PrivateRoot)?
            .canonicalize()?
            .join(root.file_name().ok_or(Error::PrivateRoot)?)
            != root
        {
            return Err(Error::PrivateRoot);
        }
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        if root.canonicalize()? != root || fs::metadata(root)?.permissions().mode() & 0o077 != 0 {
            return Err(Error::PrivateRoot);
        }
        let path = root.join("lifecycle.lock");
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| !metadata.is_file())
        {
            return Err(Error::PrivateRoot);
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        lock.lock_exclusive()?;
        Ok(Self {
            root: root.to_owned(),
            _lock: lock,
        })
    }

    /// Request supervision of the recorded identity, never the caller's newer package.
    /// Callers must separately authenticate controller readiness before admitting effects.
    pub fn start(&self, native: &Native, candidate: &Package) -> Result<Package, Error> {
        native.check()?;
        let selected = self.root.join("package.json");
        let package = match Package::load(&selected) {
            Ok(package) => package,
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => candidate.clone(),
            Err(error) => return Err(error),
        };
        native.pin(&self.root.join("roots"), &package)?;
        let mut record = tempfile::NamedTempFile::new_in(&self.root)?;
        record.write_all(&serde_json::to_vec(&package)?)?;
        record.as_file().sync_all()?;
        record.persist(selected).map_err(|error| error.error)?;
        match fs::remove_file(self.root.join("stopped")) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        native.start(&self.root, &package)?;
        Ok(package)
    }

    /// Persist intent before contacting supervision, including when its control plane fails.
    pub fn stop(&self, native: &Native) -> Result<(), Error> {
        File::create(self.root.join("stopped"))?.sync_all()?;
        native.stop()
    }
}

#[cfg(test)]
mod test;
