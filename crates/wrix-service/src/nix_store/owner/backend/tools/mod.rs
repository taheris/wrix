//! Package-selected tools and a frozen host Podman selection; never worker-supplied commands.

use super::{Error, transport};
use crate::nix_store::native::image::StorePath;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
use wrix_core::nix_store::pool::System;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(try_from = "Wire", into = "Wire")]
pub struct Tools {
    root: StorePath,
    system: Host,
    podman: Option<Box<Path>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u16,
    root: StorePath,
    system: Host,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    podman: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
enum Host {
    #[serde(rename = "x86_64-linux")]
    X86Linux,
    #[serde(rename = "aarch64-linux")]
    ArmLinux,
    #[serde(rename = "aarch64-darwin")]
    ArmDarwin,
}
impl Host {
    const fn guest(self) -> System {
        match self {
            Self::X86Linux => System::X86_64Linux,
            Self::ArmLinux | Self::ArmDarwin => System::Aarch64Linux,
        }
    }
}

impl TryFrom<Wire> for Tools {
    type Error = Error;
    fn try_from(wire: Wire) -> Result<Self, Error> {
        if !matches!(
            (wire.version, wire.podman.is_some()),
            (1, false) | (2, true)
        ) || wire.podman.as_ref().is_some_and(|path| {
            !path.is_absolute()
                || path.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
        }) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            root: wire.root,
            system: wire.system,
            podman: wire.podman.map(PathBuf::into_boxed_path),
        })
    }
}

impl From<Tools> for Wire {
    fn from(tools: Tools) -> Self {
        Self {
            version: if tools.podman.is_some() { 2 } else { 1 },
            root: tools.root,
            system: tools.system,
            podman: tools.podman.map(Path::into_path_buf),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Platform {
    Podman,
    Apple,
}

#[derive(Clone, Copy)]
pub(super) enum Program {
    Podman,
    Container,
    Launchctl,
    Ifconfig,
    Uname,
    Unmount,
    Ssh,
    Keygen,
    NixStore,
    Skopeo,
    Sshfs,
    Cat,
    Ls,
    Fusermount,
}
impl Program {
    const fn name(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Container => "container",
            Self::Launchctl => "wrix-nix-pool-launchctl",
            Self::Ifconfig => "wrix-nix-pool-ifconfig",
            Self::Uname => "wrix-nix-pool-uname",
            Self::Unmount => "wrix-nix-pool-unmount",
            Self::Ssh => "ssh",
            Self::Keygen => "ssh-keygen",
            Self::NixStore => "nix-store",
            Self::Skopeo => "skopeo",
            Self::Sshfs => "sshfs",
            Self::Cat => "cat",
            Self::Ls => "ls",
            Self::Fusermount => "fusermount3",
        }
    }
}

pub(crate) struct AppleNetworkObservation {
    pub(crate) network: Vec<u8>,
    pub(crate) interfaces: Vec<u8>,
    pub(crate) release: Vec<u8>,
}

impl Tools {
    pub(crate) fn apple_network(&self) -> Result<AppleNetworkObservation, Error> {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(Error::Unsupported);
        }
        self.check_system(System::Aarch64Linux)?;
        let mut command = self.command(Program::Container);
        command.args(["network", "inspect", "default"]);
        let budget = Some(Duration::from_secs(5));
        Ok(AppleNetworkObservation {
            network: super::success(self.run(command, None, budget)?)?,
            interfaces: super::success(self.run(self.command(Program::Ifconfig), None, budget)?)?,
            release: super::success(self.run(self.command(Program::Uname), None, budget)?)?,
        })
    }

    pub(crate) fn for_package(
        package: &crate::control::supervision::Package,
    ) -> Result<Self, Error> {
        let system = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Host::X86Linux
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Host::ArmLinux
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Host::ArmDarwin
        } else {
            return Err(Error::Unsupported);
        };
        let root = package
            .runtime()
            .to_str()
            .ok_or(Error::Invalid)?
            .to_owned()
            .try_into()?;
        let podman = if system == Host::ArmDarwin {
            None
        } else {
            Some(
                wrix_core::container_runtime::Engine::Podman
                    .path()?
                    .into_boxed_path(),
            )
        };
        Ok(Self {
            root,
            system,
            podman,
        })
    }

    pub fn from_env() -> Result<Self, Error> {
        let path = std::env::var_os("WRIX_NIX_POOL_HOST_TOOLS").ok_or(Error::Unsupported)?;
        let mut tools = Self::load(Path::new(&path))?;
        if tools.system != Host::ArmDarwin {
            tools.podman = Some(
                wrix_core::container_runtime::Engine::Podman
                    .path()?
                    .into_boxed_path(),
            );
        }
        Ok(tools)
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    fn check_system(&self, system: System) -> Result<(), Error> {
        let native = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Host::X86Linux
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Host::ArmLinux
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Host::ArmDarwin
        } else {
            return Err(Error::Unsupported);
        };
        if self.system != native || self.system.guest() != system {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    pub(crate) fn check(&self, system: System) -> Result<(), Error> {
        self.check_system(system)?;
        if self.runtime() == wrix_sandbox::image::Runtime::Container {
            super::apple::available(self)?;
            return Ok(());
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")?;
        let mut command = self.command(Program::Podman);
        command.args(["info", "--format", "json"]);
        let bytes = super::success(self.run(command, None, Some(Duration::from_secs(15)))?)?;
        let info: serde_json::Value = serde_json::from_slice(&bytes)?;
        if info["host"]["security"]["rootless"] != true {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    pub(crate) fn export_check(&self) -> Result<(), Error> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
        if self.platform() == Platform::Apple {
            if !cfg!(target_os = "macos") {
                return Err(Error::Unsupported);
            }
            let helper = std::fs::metadata(
                "/Library/Filesystems/macfuse.fs/Contents/Resources/mount_macfuse",
            )?;
            if !helper.is_file()
                || helper.uid() != 0
                || helper.mode() & 0o022 != 0
                || helper.mode() & 0o111 == 0
                || !Path::new(self.command(Program::Sshfs).get_program()).is_file()
            {
                return Err(Error::Unsupported);
            }
            return Ok(());
        }
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported);
        }
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open("/dev/fuse")?;
        if !device.metadata()?.file_type().is_char_device() {
            return Err(Error::Unsupported);
        }
        if !Path::new(self.command(Program::Sshfs).get_program()).is_file() {
            return Err(Error::Unsupported);
        }
        Self::unmount_helper()?;
        Ok(())
    }

    pub(crate) fn view_check(
        &self,
        plane: wrix_state::nix_store::admission::request::Plane,
    ) -> Result<(), Error> {
        use std::os::unix::fs::PermissionsExt;
        self.check_system(self.system.guest())?;
        if (plane == wrix_state::nix_store::admission::request::Plane::Apple)
            != (self.platform() == Platform::Apple)
        {
            return Err(Error::Unsupported);
        }
        self.export_check()?;
        if plane.is_podman_namespace() {
            for program in [Program::Cat, Program::Ls, Program::Fusermount] {
                let metadata = match std::fs::metadata(self.command(program).get_program()) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(Error::Unsupported);
                    }
                    Err(error) => return Err(error.into()),
                };
                if !metadata.is_file()
                    || metadata.permissions().mode() & 0o111 == 0
                    || metadata.permissions().mode() & 0o022 != 0
                {
                    return Err(Error::Unsupported);
                }
            }
        }
        if plane == wrix_state::nix_store::admission::request::Plane::Krun {
            let metadata = std::fs::metadata(self.vm())?;
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o111 == 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return Err(Error::Unsupported);
            }
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/kvm")?;
        }
        Ok(())
    }

    pub(crate) fn apple_image_commands(&self) -> Result<(Command, Command), Error> {
        self.check_system(self.system.guest())?;
        if self.platform() != Platform::Apple {
            return Err(Error::Unsupported);
        }
        Ok((
            self.command(Program::Container),
            self.command(Program::Skopeo),
        ))
    }

    pub(crate) fn belongs_to(&self, package: &crate::control::supervision::Package) -> bool {
        Path::new(self.root.as_str()) == package.runtime()
            && self.podman.as_deref().is_none_or(|recorded| {
                wrix_core::container_runtime::Engine::Podman
                    .path()
                    .is_ok_and(|selected| selected == recorded)
            })
    }

    pub(super) const fn platform(&self) -> Platform {
        match self.system {
            Host::ArmDarwin => Platform::Apple,
            Host::X86Linux | Host::ArmLinux => Platform::Podman,
        }
    }

    pub(super) const fn image_runtime(&self) -> wrix_sandbox::image::Runtime {
        self.runtime()
    }

    pub(super) const fn runtime(&self) -> wrix_sandbox::image::Runtime {
        match self.system {
            Host::X86Linux | Host::ArmLinux => wrix_sandbox::image::Runtime::Podman,
            Host::ArmDarwin => wrix_sandbox::image::Runtime::Container,
        }
    }

    pub(super) fn attach(&self) -> PathBuf {
        Path::new(self.root.as_str()).join("bin/wrix-nix-pool-attach")
    }

    pub(super) fn vm(&self) -> PathBuf {
        Path::new(self.root.as_str()).join("bin/krun")
    }

    pub(super) fn command(&self, program: Program) -> Command {
        let path = if matches!(program, Program::Podman) {
            self.podman.as_ref().map_or_else(
                || Path::new(self.root.as_str()).join("bin/podman"),
                |path| path.to_path_buf(),
            )
        } else {
            Path::new(self.root.as_str())
                .join("bin")
                .join(program.name())
        };
        self.configure(Command::new(path))
    }

    pub(super) fn view_command(
        &self,
        plane: wrix_state::nix_store::admission::request::Plane,
        program: Program,
    ) -> Command {
        match plane {
            wrix_state::nix_store::admission::request::Plane::Host
            | wrix_state::nix_store::admission::request::Plane::Apple => self.command(program),
            wrix_state::nix_store::admission::request::Plane::Podman
            | wrix_state::nix_store::admission::request::Plane::Krun => {
                let mut command = self.command(Program::Podman);
                command
                    .arg("unshare")
                    .arg(self.command(program).get_program());
                command
            }
        }
    }

    fn unmount_helper() -> Result<PathBuf, Error> {
        use std::os::unix::fs::MetadataExt;
        for path in [
            "/run/wrappers/bin/fusermount3",
            "/usr/bin/fusermount3",
            "/bin/fusermount3",
        ] {
            let metadata = match std::fs::metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file()
                || metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
                || metadata.mode() & 0o4000 == 0
                || metadata.mode() & 0o111 == 0
            {
                return Err(Error::Unsupported);
            }
            return Ok(path.into());
        }
        Err(Error::Unsupported)
    }

    pub(super) fn unmount(
        &self,
        plane: wrix_state::nix_store::admission::request::Plane,
        projection: &Path,
    ) -> Result<Output, Error> {
        let mut command = match plane {
            wrix_state::nix_store::admission::request::Plane::Apple => {
                if self.platform() != Platform::Apple {
                    return Err(Error::Unsupported);
                }
                let mut command = self.command(Program::Unmount);
                command.arg(projection);
                return self.run(command, None, Some(Duration::from_secs(30)));
            }
            wrix_state::nix_store::admission::request::Plane::Host => {
                self.configure(Command::new(Self::unmount_helper()?))
            }
            wrix_state::nix_store::admission::request::Plane::Podman
            | wrix_state::nix_store::admission::request::Plane::Krun => {
                self.view_command(plane, Program::Fusermount)
            }
        };
        command.arg("-u").arg(projection);
        Ok(transport::capture(
            command,
            None,
            Some(Duration::from_secs(30)),
        )?)
    }

    fn configure(&self, mut command: Command) -> Command {
        command
            .current_dir("/")
            .env_clear()
            .env(
                "PATH",
                format!("{}/bin:/run/wrappers/bin:/usr/bin:/bin", self.root.as_str()),
            )
            .env("LC_ALL", "C");
        for name in [
            "HOME",
            "USER",
            "LOGNAME",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "DBUS_SESSION_BUS_ADDRESS",
            "CONTAINERS_CONF",
            "CONTAINERS_CONF_OVERRIDE",
            "CONTAINERS_STORAGE_CONF",
            "CONTAINERS_STORAGE_CONF_OVERRIDE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    pub(super) fn run(
        &self,
        command: Command,
        input: Option<Vec<u8>>,
        budget: Option<Duration>,
    ) -> Result<Output, Error> {
        if !Path::new(command.get_program()).starts_with(self.root.as_str())
            && self.podman.as_deref() != Some(Path::new(command.get_program()))
        {
            return Err(Error::Invalid);
        }
        Ok(transport::capture(command, input, budget)?)
    }

    pub(super) fn export(
        &self,
        paths: &[StorePath],
        file: &std::fs::File,
        limit: u64,
    ) -> Result<(std::process::ExitStatus, u64), Error> {
        let mut command = self.command(Program::NixStore);
        command
            .arg("--export")
            .args(paths.iter().map(StorePath::as_str));
        let archive = transport::export(command, file.try_clone()?, limit)?;
        tracing::debug!(operation = "nix_image_export", diagnostic_bytes = archive.stderr.len(), status = %archive.status, "fixed image export finished");
        Ok((archive.status, archive.bytes))
    }

    pub(super) fn pin(&self, root: &Path) -> Result<(), Error> {
        let mut command = self.command(Program::NixStore);
        command
            .arg("--realise")
            .arg(self.root.as_str())
            .arg("--add-root")
            .arg(root.join("host-runtime"))
            .arg("--indirect");
        super::success(self.run(command, None, None)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn frozen_host_engine_roundtrips_without_rebinding_legacy_recipes() {
        let legacy: Tools = serde_json::from_value(serde_json::json!({
            "version": 1, "root": "/nix/store/00000000000000000000000000000000-runtime",
            "system": "x86_64-linux"
        }))
        .unwrap();
        let mut selected = legacy.clone();
        selected.podman = Some(PathBuf::from("/host/bin/podman").into_boxed_path());
        let value = serde_json::to_value(&selected).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(value["podman"], "/host/bin/podman");
        let restored: Tools = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            restored.command(Program::Podman).get_program(),
            "/host/bin/podman"
        );
        for (version, path) in [(1, "/host/bin/podman"), (2, "podman")] {
            let mut invalid = value.clone();
            invalid["version"] = serde_json::json!(version);
            invalid["podman"] = serde_json::json!(path);
            assert!(serde_json::from_value::<Tools>(invalid).is_err());
        }
        assert_eq!(serde_json::to_value(&legacy).unwrap()["version"], 1);
        assert_eq!(
            legacy.command(Program::Podman).get_program(),
            Path::new(legacy.root.as_str()).join("bin/podman")
        );
    }

    #[test]
    fn architecture_admission_rejects_foreign_tools_without_executing_or_allocating() {
        for system in [System::X86_64Linux, System::Aarch64Linux] {
            let tools: Tools = serde_json::from_value(serde_json::json!({
                "version":1,
                "root":"/nix/store/00000000000000000000000000000000-not-installed",
                "system":system,
            }))
            .unwrap();
            let native = match system {
                System::X86_64Linux => cfg!(all(target_os = "linux", target_arch = "x86_64")),
                System::Aarch64Linux => cfg!(all(target_os = "linux", target_arch = "aarch64")),
            };
            assert_eq!(tools.check_system(system).is_ok(), native);
            let peer = match system {
                System::X86_64Linux => System::Aarch64Linux,
                System::Aarch64Linux => System::X86_64Linux,
            };
            assert!(tools.check_system(peer).is_err());
        }
    }
    #[test]
    fn darwin_tools_select_native_apple_programs_and_a_linux_guest_without_podman() {
        let tools: Tools = serde_json::from_value(serde_json::json!({"version":1,"root":"/nix/store/00000000000000000000000000000000-runtime","system":"aarch64-darwin"})).unwrap();
        assert_eq!(tools.platform(), Platform::Apple);
        assert_eq!(
            tools.image_runtime(),
            wrix_sandbox::image::Runtime::Container
        );
        assert_eq!(tools.system.guest(), System::Aarch64Linux);
        assert_eq!(
            tools.command(Program::Container).get_program(),
            Path::new("/nix/store/00000000000000000000000000000000-runtime/bin/container")
        );
        assert_eq!(
            tools.check_system(System::Aarch64Linux).is_ok(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        assert!(tools.check_system(System::X86_64Linux).is_err());
    }

    #[test]
    fn host_program_definition_is_closed_and_does_not_accept_executables_or_mounts() {
        let root = "/nix/store/00000000000000000000000000000000-runtime";
        let value = serde_json::json!({"version":1,"root":root,"system":"x86_64-linux"});
        let tools: Tools = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(tools.vm(), Path::new(root).join("bin/krun"));
        for field in ["command", "podman", "mount", "endpoint"] {
            let mut invalid = value.clone();
            invalid[field] = serde_json::json!("/tmp/arbitrary");
            assert!(serde_json::from_value::<Tools>(invalid).is_err());
        }
    }
}
