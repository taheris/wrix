//! Caller-independent user supervision; only host package metadata selects executable closures.

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fmt::Write as _,
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
    /// host controller unit conflicts with existing user supervision; stop and reconcile the existing controller before retrying: {0}
    UnitConflict(String),
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

impl<'de> Deserialize<'de> for Package {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_wire(Wire::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Package {
    /// Read the immutable binding set by the host package wrapper, not project configuration.
    pub fn from_env() -> Result<Self, Error> {
        let path = std::env::var_os("WRIX_CONTROLLER_PACKAGE_BINDING").ok_or(Error::Package)?;
        Self::load(Path::new(&path))
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::from_wire(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn from_wire(wire: Wire) -> Result<Self, Error> {
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

    pub(crate) fn runtime(&self) -> &Path {
        &self.runtime
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
    pub const fn new(
        platform: Platform,
        uid: u32,
        supervisor: PathBuf,
        nix_store: PathBuf,
    ) -> Self {
        Self {
            platform,
            supervisor,
            nix_store,
            uid,
        }
    }

    pub fn for_package(platform: Platform, uid: u32, package: &Package) -> Self {
        Self::new(
            platform,
            uid,
            match platform {
                Platform::Linux => package.runtime.join("bin/systemctl"),
                Platform::Darwin => "/bin/launchctl".into(),
            },
            package.runtime.join("bin/nix-store"),
        )
    }

    fn command(&self, args: &[&std::ffi::OsStr]) -> Result<(), Error> {
        self.output(args).map(|_| ())
    }

    fn output(&self, args: &[&std::ffi::OsStr]) -> Result<String, Error> {
        let output = Command::new(&self.supervisor)
            .args(args)
            .output()
            .map_err(|error| Error::Supervisor(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::Supervisor(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn link_unit(&self, unit: &Path) -> Result<(), Error> {
        match self.command(&["--user".as_ref(), "link".as_ref(), unit.as_os_str()]) {
            Ok(()) => Ok(()),
            Err(Error::Supervisor(message)) => {
                let state = self.output(&[
                    "--user".as_ref(),
                    "show".as_ref(),
                    "--property=LoadState,ActiveState,FragmentPath".as_ref(),
                    "wrix-host-controller.service".as_ref(),
                ])?;
                if ![
                    "LoadState=not-found",
                    "ActiveState=inactive",
                    "FragmentPath=",
                ]
                .iter()
                .all(|expected| state.lines().any(|line| line == *expected))
                {
                    return Err(Error::UnitConflict(message));
                }
                self.command(&[
                    "--user".as_ref(),
                    "link".as_ref(),
                    "--force".as_ref(),
                    unit.as_os_str(),
                ])
            }
            Err(error) => Err(error),
        }
    }

    fn check(&self) -> Result<(), Error> {
        match self.platform {
            Platform::Linux => self.command(&["--user".as_ref(), "show-environment".as_ref()]),
            Platform::Darwin => {
                self.command(&["print".as_ref(), format!("gui/{}", self.uid).as_ref()])
            }
        }
    }

    /// Materialize the new closure before taking controller lifecycle exclusion:
    /// a host post-build hook may itself require the still-running controller.
    pub fn prepare_upgrade(&self, root: &Path, package: &Package) -> Result<(), Error> {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir()
            || root.canonicalize()? != root
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::PrivateRoot);
        }
        self.check()?;
        self.pin(&root.join("maintenance-roots"), package)
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
                let mut contents = unit_contents(root, package);
                for (name, value) in host_runtime_environment()? {
                    let mut assignment = std::ffi::OsString::from(format!("{name}="));
                    assignment.push(value);
                    writeln!(
                        contents,
                        "Environment={}",
                        systemd_path(Path::new(&assignment))
                    )
                    .map_err(io::Error::other)?;
                }
                fs::write(&unit, contents)?;
                self.link_unit(&unit)?;
                self.command(&["--user".as_ref(), "daemon-reload".as_ref()])?;
                self.command(&[
                    "--user".as_ref(),
                    "start".as_ref(),
                    "wrix-host-controller.service".as_ref(),
                ])
            }
            Platform::Darwin => {
                let plist = root.join("org.wrix.host-controller.plist");
                let mut environment = String::new();
                for (name, value) in host_runtime_environment()? {
                    write!(
                        environment,
                        "<key>{name}</key><string>{}</string>",
                        xml(Path::new(&value))?
                    )
                    .map_err(io::Error::other)?;
                }
                let contents = format!(
                    "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>org.wrix.host-controller</string><key>ProgramArguments</key><array><string>{}</string><string>{}</string></array><key>EnvironmentVariables</key><dict>{environment}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict></dict></plist>",
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

    fn verify_owned(&self, root: &Path, package: &Package) -> Result<bool, Error> {
        match self.platform {
            Platform::Linux => {
                let state = self.output(&[
                    "--user".as_ref(),
                    "show".as_ref(),
                    "--property=LoadState,ActiveState,FragmentPath".as_ref(),
                    "wrix-host-controller.service".as_ref(),
                ])?;
                if [
                    "LoadState=not-found",
                    "ActiveState=inactive",
                    "FragmentPath=",
                ]
                .iter()
                .all(|expected| state.lines().any(|line| line == *expected))
                {
                    return Ok(false);
                }
                let unit = root.join("wrix-host-controller.service");
                // `systemctl link` normally reports the user-config symlink,
                // not its retained private target. Verify that exact target,
                // never accept another unit merely because its text matches.
                let owned_fragment = state
                    .lines()
                    .find_map(|line| line.strip_prefix("FragmentPath="))
                    .is_some_and(|path| {
                        Path::new(path).is_absolute()
                            && Path::new(path)
                                .canonicalize()
                                .is_ok_and(|target| target == unit)
                    });
                if !state.lines().any(|line| line == "LoadState=loaded")
                    || !owned_fragment
                    || !fs::symlink_metadata(&unit)?.is_file()
                    || !unit_matches(root, package, &fs::read_to_string(&unit)?)
                {
                    return Err(Error::UnitConflict(state));
                }
            }
            Platform::Darwin => {
                let service = format!("gui/{}/org.wrix.host-controller", self.uid);
                let output = Command::new(&self.supervisor)
                    .args(["print", &service])
                    .output()?;
                if !output.status.success() {
                    let message = String::from_utf8_lossy(&output.stderr);
                    if message.contains("Could not find service") {
                        self.check()?;
                        return Ok(false);
                    }
                    return Err(Error::Supervisor(message.into_owned()));
                }
                let state = String::from_utf8_lossy(&output.stdout);
                let path = format!(
                    "path = {}",
                    root.join("org.wrix.host-controller.plist").display()
                );
                let program = format!("program = {}", package.executable().display());
                if ![path, program]
                    .iter()
                    .all(|expected| state.lines().any(|line| line.trim() == expected))
                {
                    return Err(Error::UnitConflict(state.into_owned()));
                }
            }
        }
        Ok(true)
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

// Supervisors do not inherit the invoking shell's runtime/storage configuration.
fn host_runtime_environment() -> Result<Vec<(&'static str, std::ffi::OsString)>, Error> {
    let mut environment = Vec::new();
    if std::env::var_os("WRIX_CONTAINER_RUNTIME").is_some() {
        environment.push((
            "WRIX_CONTAINER_RUNTIME",
            wrix_core::container_runtime::Engine::current()
                .path()?
                .into_os_string(),
        ));
    }
    for &name in RUNTIME_ENVIRONMENT.iter().skip(1) {
        if let Some(value) = std::env::var_os(name) {
            environment.push((name, value));
        }
    }
    Ok(environment)
}

const RUNTIME_ENVIRONMENT: &[&str] = &[
    "WRIX_CONTAINER_RUNTIME",
    "WRIX_HOST_RUNTIME_PATH",
    "CONTAINERS_CONF",
    "CONTAINERS_CONF_OVERRIDE",
    "CONTAINERS_STORAGE_CONF",
    "CONTAINER_HOST",
    "CONTAINER_CONNECTION",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
];

fn unit_matches(root: &Path, package: &Package, contents: &str) -> bool {
    let Some(environment) = contents.strip_prefix(&unit_contents(root, package)) else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    environment.lines().all(|line| {
        RUNTIME_ENVIRONMENT.iter().any(|&name| {
            let assignment = systemd_path(Path::new(&format!("{name}=")));
            let prefix = format!("Environment={}", assignment.trim_end_matches('"'));
            let Some(value) = line
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix('"'))
            else {
                return false;
            };
            let mut remaining = value.as_bytes();
            while !remaining.is_empty() {
                if let Some(tail) = remaining.strip_prefix(b"%%") {
                    remaining = tail;
                } else if remaining.len() >= 4
                    && remaining.starts_with(b"\\x")
                    && remaining[2..4].iter().all(u8::is_ascii_hexdigit)
                {
                    remaining = &remaining[4..];
                } else {
                    return false;
                }
            }
            seen.insert(name)
        })
    })
}

fn unit_contents(root: &Path, package: &Package) -> String {
    // Controller loss is not worker exit: export helpers retain their native readers until checked retirement.
    format!(
        "[Unit]\nDescription=Wrix host controller\n[Service]\nType=exec\nExecStart={} {}\nRestart=on-failure\nRestartSec=1\nKillMode=process\n",
        systemd_path(&package.executable()),
        systemd_path(root)
    )
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

    /// Admit native unit ownership before any backend creation or data effects.
    pub fn preflight(
        &self,
        native: &Native,
        candidate: &Package,
    ) -> Result<Option<Package>, Error> {
        native.check()?;
        match Package::load(&self.root.join("package.json")) {
            Ok(recorded) => {
                native.verify_owned(&self.root, &recorded)?;
                Ok(Some(recorded))
            }
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                if native.verify_owned(&self.root, candidate)? {
                    return Err(Error::UnitConflict(
                        "an existing unit has no retained package identity".into(),
                    ));
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
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

    /// Explicit maintenance only: retain the old package until native stop and
    /// policy migration succeed. Ordinary admission never calls this operation.
    pub fn upgrade(
        &self,
        native: &Native,
        candidate: &Package,
        migrate: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), Error> {
        native.check()?;
        let previous = match Package::load(&self.root.join("package.json")) {
            Ok(package) => package,
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                migrate()?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        native.pin(&self.root.join("maintenance-roots"), candidate)?;
        if native.verify_owned(&self.root, &previous)? {
            self.stop(native)?;
        }
        migrate()?;
        let mut record = tempfile::NamedTempFile::new_in(&self.root)?;
        record.write_all(&serde_json::to_vec(candidate)?)?;
        record.as_file().sync_all()?;
        record
            .persist(self.root.join("package.json"))
            .map_err(|error| error.error)?;
        File::open(&self.root)?.sync_all()?;
        self.start(native, candidate)?;
        Ok(())
    }

    /// Stop only the verified recorded native unit. An absent unit still retains shutdown intent.
    pub fn stop_owned(&self, native: &Native) -> Result<(), Error> {
        let package = Package::load(&self.root.join("package.json"))?;
        let owned = native.verify_owned(&self.root, &package)?;
        self.record_stop()?;
        if owned {
            native.stop()?;
        }
        Ok(())
    }

    fn record_stop(&self) -> Result<(), Error> {
        let mut marker = tempfile::NamedTempFile::new_in(&self.root)?;
        marker.write_all(b"explicit host shutdown\n")?;
        marker.as_file().sync_all()?;
        marker
            .persist(self.root.join("stopped"))
            .map_err(|error| error.error)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    /// Persist intent before contacting supervision, including when its control plane fails.
    pub fn stop(&self, native: &Native) -> Result<(), Error> {
        self.record_stop()?;
        native.stop()
    }
}

#[cfg(test)]
mod test;
