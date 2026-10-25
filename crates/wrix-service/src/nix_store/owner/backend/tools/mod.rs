//! Fixed package-selected host programs; project and worker runtime selectors are not inputs.

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
    system: System,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u16,
    root: StorePath,
    system: System,
}

impl TryFrom<Wire> for Tools {
    type Error = Error;
    fn try_from(wire: Wire) -> Result<Self, Error> {
        if wire.version != 1 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            root: wire.root,
            system: wire.system,
        })
    }
}

impl From<Tools> for Wire {
    fn from(tools: Tools) -> Self {
        Self {
            version: 1,
            root: tools.root,
            system: tools.system,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Program {
    Podman,
    Ssh,
    Keygen,
    NixStore,
    Sshfs,
    Cat,
    Ls,
    Fusermount,
}
impl Program {
    const fn name(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Ssh => "ssh",
            Self::Keygen => "ssh-keygen",
            Self::NixStore => "nix-store",
            Self::Sshfs => "sshfs",
            Self::Cat => "cat",
            Self::Ls => "ls",
            Self::Fusermount => "fusermount3",
        }
    }
}

impl Tools {
    pub fn from_env() -> Result<Self, Error> {
        let path = std::env::var_os("WRIX_NIX_POOL_HOST_TOOLS").ok_or(Error::Unsupported)?;
        Self::load(Path::new(&path))
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    pub(crate) fn check(&self, system: System) -> Result<(), Error> {
        let native = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            System::X86_64Linux
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            System::Aarch64Linux
        } else {
            return Err(Error::Unsupported);
        };
        if self.system != native || self.system != system {
            return Err(Error::Unsupported);
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
        use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
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
        self.export_check()?;
        if plane == wrix_state::nix_store::admission::request::Plane::Podman {
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
        Ok(())
    }

    pub(super) fn vm(&self) -> PathBuf {
        Path::new(self.root.as_str()).join("bin/krun")
    }

    pub(super) fn command(&self, program: Program) -> Command {
        self.configure(Command::new(
            Path::new(self.root.as_str())
                .join("bin")
                .join(program.name()),
        ))
    }

    pub(super) fn view_command(
        &self,
        plane: wrix_state::nix_store::admission::request::Plane,
        program: Program,
    ) -> Command {
        match plane {
            wrix_state::nix_store::admission::request::Plane::Host => self.command(program),
            wrix_state::nix_store::admission::request::Plane::Podman => {
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
            wrix_state::nix_store::admission::request::Plane::Host => {
                self.configure(Command::new(Self::unmount_helper()?))
            }
            wrix_state::nix_store::admission::request::Plane::Podman => {
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
        if !Path::new(command.get_program()).starts_with(self.root.as_str()) {
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
