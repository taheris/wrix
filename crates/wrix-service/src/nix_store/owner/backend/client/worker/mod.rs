//! Fixed Podman worker ingress; only execution keys and pinned pool identity cross the worker boundary.
use super::{Delivery, Error, Record, State, check_directory, mount, private_file};
use std::{
    fs::{self, File},
    io::{Read, Write},
    num::NonZeroU16,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const DIRECTORY: &str = "/run/wrix/nix-pool";
pub const STORE: &str = "unix:///run/wrix/nix-client/daemon.sock";
const FILES: [&str; 5] = ["key", "known-hosts", "ssh_config", "nix.conf", "version"];

fn configuration_on(
    port: NonZeroU16,
    plane: wrix_state::nix_store::admission::request::Plane,
) -> String {
    let configuration = configuration(port);
    if plane == wrix_state::nix_store::admission::request::Plane::Apple {
        configuration
            .replace("HostName 169.254.1.2", "HostName 127.0.0.1")
            .replace(
                "ProxyCommand none",
                "ProxyCommand /etc/wrix/apple-nix-bridge connect",
            )
    } else {
        configuration
    }
}
fn configuration(port: NonZeroU16) -> String {
    format!(
        "Host wrix-nix-pool\n HostName 169.254.1.2\n Port {port}\n User nix-client\n IdentityFile {DIRECTORY}/key\n UserKnownHostsFile {DIRECTORY}/known-hosts\n GlobalKnownHostsFile /dev/null\n HostKeyAlias [127.0.0.1]:{port}\n StrictHostKeyChecking yes\n BatchMode yes\n IdentitiesOnly yes\n IdentityAgent none\n ClearAllForwardings yes\n RequestTTY no\n PermitLocalCommand no\n ProxyCommand none\n ControlMaster no\n"
    )
}
fn read(root: &Path, name: &str) -> Result<Vec<u8>, Error> {
    let file = private_file(root, name, false)?;
    if file.metadata()?.len() > 16_384 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes)?;
    if bytes.len() > 16_384 {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
fn write(root: &Path, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut file = private_file(root, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn nix_configuration() -> String {
    format!(
        "store = {STORE}\nexperimental-features = nix-command flakes\naccept-flake-config = false\n"
    )
}

#[cfg(test)]
pub(super) fn create(root: &Path, port: NonZeroU16) -> Result<(), Error> {
    create_on(
        root,
        port,
        wrix_state::nix_store::admission::request::Plane::Podman,
    )
}
pub(super) fn create_on(
    root: &Path,
    port: NonZeroU16,
    plane: wrix_state::nix_store::admission::request::Plane,
) -> Result<(), Error> {
    let key = read(root, "key")?;
    let known = read(root, "known-hosts")?;
    let directory = root.join("worker");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let state = directory.join("state");
    fs::DirBuilder::new().mode(0o700).create(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755))?;
    write(&directory, "key", &key)?;
    write(&directory, "known-hosts", &known)?;
    write(
        &directory,
        "ssh_config",
        configuration_on(port, plane).as_bytes(),
    )?;
    write(&directory, "nix.conf", nix_configuration().as_bytes())?;
    write(
        &directory,
        "version",
        if plane == wrix_state::nix_store::admission::request::Plane::Apple {
            b"2\n"
        } else {
            b"1\n"
        },
    )?;
    File::open(&directory)?.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
fn inspect(root: &Path, port: NonZeroU16) -> Result<PathBuf, Error> {
    inspect_on(
        root,
        port,
        wrix_state::nix_store::admission::request::Plane::Podman,
    )
}
fn inspect_on(
    root: &Path,
    port: NonZeroU16,
    plane: wrix_state::nix_store::admission::request::Plane,
) -> Result<PathBuf, Error> {
    let directory = root.join("worker");
    check_directory(&directory)?;
    let names: std::collections::BTreeSet<_> = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    let expected: std::collections::BTreeSet<_> = FILES
        .iter()
        .copied()
        .chain(["state"])
        .map(std::ffi::OsString::from)
        .collect();
    let state = directory.join("state");
    let metadata = fs::symlink_metadata(&state)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o755
        || fs::read_dir(&state)?.next().transpose()?.is_some()
    {
        return Err(Error::Retained);
    }
    if names != expected
        || read(&directory, "version")?
            != if plane == wrix_state::nix_store::admission::request::Plane::Apple {
                b"2\n"
            } else {
                b"1\n"
            }
        || read(&directory, "ssh_config")? != configuration_on(port, plane).as_bytes()
        || read(&directory, "nix.conf")? != nix_configuration().as_bytes()
        || read(&directory, "key")? != read(root, "key")?
        || read(&directory, "known-hosts")? != read(root, "known-hosts")?
    {
        return Err(Error::Retained);
    }
    Ok(directory)
}
impl Delivery {
    pub(super) fn tools(&self) -> Result<super::super::Tools, Error> {
        let host = wrix_state::host::Host::discover().map_err(std::io::Error::other)?;
        let control = host
            .roots()
            .state()
            .join("control/nix")
            .join(self.pool.repository.to_string())
            .join(self.pool.system.as_str());
        if self.key
            != control
                .join("executions")
                .join(self.ticket.as_str())
                .join("key")
        {
            return Err(Error::Invalid);
        }
        let deployment = crate::control::existing_deployment(&host)
            .map_err(std::io::Error::other)?
            .ok_or(Error::Retained)?;
        let record = super::super::Record::read(&control, &deployment, &self.pool)?;
        if !record.executions.contains(&self.ticket) {
            return Err(Error::Retained);
        }
        let super::super::State::Serving { connection } = record.state else {
            return Err(Error::Retained);
        };
        if self.plane == wrix_state::nix_store::admission::request::Plane::Apple
            && connection.port != self.port
        {
            return Err(Error::Retained);
        }
        Ok(connection.target.attempt.software.tools)
    }
    /// Use the protected owner's exact package and rootless namespace environment, never an ambient PATH executable.
    pub fn podman(&self) -> Result<std::process::Command, Error> {
        self.podman_configuration()?;
        Ok(self.tools()?.command(super::super::Program::Podman))
    }
    /// The recorded worker runtime, never an ambient runtime alias or caller-chosen executable.
    pub fn krun_runtime(&self) -> Result<Option<PathBuf>, Error> {
        match self.plane {
            wrix_state::nix_store::admission::request::Plane::Krun => Ok(Some(self.tools()?.vm())),
            wrix_state::nix_store::admission::request::Plane::Podman
            | wrix_state::nix_store::admission::request::Plane::Apple => Ok(None),
            wrix_state::nix_store::admission::request::Plane::Host => Err(Error::Invalid),
        }
    }
    /// Validates existing fixed Podman configuration and the exact live readonly projection, without creation or repair.
    pub fn podman_configuration(&self) -> Result<PathBuf, Error> {
        if !self.plane.is_podman_namespace() {
            return Err(Error::Invalid);
        }
        self.configuration()
    }
    pub fn apple_configuration(&self) -> Result<PathBuf, Error> {
        if self.plane != wrix_state::nix_store::admission::request::Plane::Apple {
            return Err(Error::Invalid);
        }
        self.configuration()
    }
    pub fn apple(&self) -> Result<std::process::Command, Error> {
        self.apple_configuration()?;
        Ok(self.tools()?.command(super::super::Program::Container))
    }
    pub fn start_apple_bridge(
        &self,
        worker: &wrix_sandbox::execution::apple::Worker,
    ) -> Result<super::bridge::Bridge, Error> {
        super::bridge::broker::start(self, worker)
    }
    fn configuration(&self) -> Result<PathBuf, Error> {
        let root = self.key.parent().ok_or(Error::Invalid)?;
        let record = Record::read(root)?;
        if record.plane != self.plane
            || record.pool != self.pool
            || record.ticket != self.ticket
            || record.lease != self.lease
            || self.store != root.join("store")
            || self.known_hosts != root.join("known-hosts")
            || (self.plane == wrix_state::nix_store::admission::request::Plane::Apple
                && record.port != Some(self.port))
        {
            return Err(Error::Invalid);
        }
        let State::Attached { mount: expected } = record.state else {
            return Err(Error::Retained);
        };
        let tools = self.tools()?;
        if mount::observe(record.plane, &tools, &self.store, &self.lease)?.as_ref()
            != Some(&expected)
        {
            return Err(Error::Retained);
        }
        inspect_on(root, self.port, self.plane)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        write(root.path(), "key", b"execution-key").unwrap();
        write(root.path(), "known-hosts", b"pinned-key").unwrap();
        root
    }
    #[test]
    fn apple_ssh_uses_local_bridge_with_the_original_loopback_server_pin() {
        use wrix_state::nix_store::admission::request::Plane;
        let root = fixture();
        let port = NonZeroU16::new(2222).unwrap();
        create_on(root.path(), port, Plane::Apple).unwrap();
        assert!(inspect_on(root.path(), port, Plane::Apple).is_ok());
        assert!(inspect_on(root.path(), port, Plane::Podman).is_err());
        let configuration =
            String::from_utf8(read(&root.path().join("worker"), "ssh_config").unwrap()).unwrap();
        assert!(configuration.contains("ProxyCommand /etc/wrix/apple-nix-bridge connect"));
        assert!(configuration.contains("HostKeyAlias [127.0.0.1]:2222"));
        assert!(!configuration.contains("169.254.1.2"));
        assert!(!configuration.contains("host.container.internal"));
        assert!(configuration.contains("StrictHostKeyChecking yes"));
    }
    #[test]
    fn fixed_worker_configuration_is_exact_and_never_repaired_or_replaced() {
        let root = fixture();
        let port = NonZeroU16::new(2222).unwrap();
        assert!(inspect(root.path(), port).is_err());
        assert!(!root.path().join("worker").exists());
        create(root.path(), port).unwrap();
        assert!(inspect(root.path(), port).is_ok());
        assert!(create(root.path(), port).is_err());
        fs::write(
            root.path().join("worker/ssh_config"),
            "ProxyCommand arbitrary",
        )
        .unwrap();
        assert!(inspect(root.path(), port).is_err());
    }
    #[test]
    fn private_state_cover_must_stay_empty_and_unknown_payload_is_preserved() {
        let root = fixture();
        let port = NonZeroU16::new(2222).unwrap();
        create(root.path(), port).unwrap();
        let state = root.path().join("worker/state");
        assert!(fs::read_dir(&state).unwrap().next().is_none());
        fs::write(state.join("db.sqlite"), b"unknown retained data").unwrap();
        assert!(inspect(root.path(), port).is_err());
        assert_eq!(
            fs::read(state.join("db.sqlite")).unwrap(),
            b"unknown retained data"
        );
    }
    #[test]
    fn private_state_cover_cannot_be_substituted_with_a_directory_alias() {
        let root = fixture();
        let port = NonZeroU16::new(2222).unwrap();
        create(root.path(), port).unwrap();
        let state = root.path().join("worker/state");
        let original = root.path().join("original-state");
        fs::rename(&state, &original).unwrap();
        std::os::unix::fs::symlink(&original, &state).unwrap();
        assert!(inspect(root.path(), port).is_err());
        assert!(original.is_dir());
    }
    #[test]
    fn worker_configuration_rejects_extra_entries_and_substituted_keys() {
        let root = fixture();
        let port = NonZeroU16::new(2222).unwrap();
        create(root.path(), port).unwrap();
        fs::write(root.path().join("worker/extra"), b"unknown").unwrap();
        assert!(inspect(root.path(), port).is_err());
        fs::remove_file(root.path().join("worker/extra")).unwrap();
        fs::remove_file(root.path().join("worker/key")).unwrap();
        std::os::unix::fs::symlink(root.path().join("key"), root.path().join("worker/key"))
            .unwrap();
        assert!(inspect(root.path(), port).is_err());
    }
}
