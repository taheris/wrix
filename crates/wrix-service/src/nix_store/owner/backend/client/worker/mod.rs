//! Fixed Podman worker ingress; only execution keys and pinned pool identity cross the worker boundary.
use super::{Delivery, Error, Record, State, check_directory, mount, private_file};
use std::{
    fs::{self, File},
    io::{Read, Write},
    num::NonZeroU16,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

pub const DIRECTORY: &str = "/run/wrix/nix-pool";
pub const STORE: &str = "unix:///run/wrix/nix-client/daemon.sock";
const FILES: [&str; 5] = ["key", "known-hosts", "ssh_config", "nix.conf", "version"];

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

pub(super) fn create(root: &Path, port: NonZeroU16) -> Result<(), Error> {
    let key = read(root, "key")?;
    let known = read(root, "known-hosts")?;
    let directory = root.join("worker");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    write(&directory, "key", &key)?;
    write(&directory, "known-hosts", &known)?;
    write(&directory, "ssh_config", configuration(port).as_bytes())?;
    write(&directory, "nix.conf", nix_configuration().as_bytes())?;
    write(&directory, "version", b"1\n")?;
    File::open(&directory)?.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}
fn inspect(root: &Path, port: NonZeroU16) -> Result<PathBuf, Error> {
    let directory = root.join("worker");
    check_directory(&directory)?;
    let names: std::collections::BTreeSet<_> = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    let expected: std::collections::BTreeSet<_> =
        FILES.iter().map(std::ffi::OsString::from).collect();
    if names != expected
        || read(&directory, "version")? != b"1\n"
        || read(&directory, "ssh_config")? != configuration(port).as_bytes()
        || read(&directory, "nix.conf")? != nix_configuration().as_bytes()
        || read(&directory, "key")? != read(root, "key")?
        || read(&directory, "known-hosts")? != read(root, "known-hosts")?
    {
        return Err(Error::Retained);
    }
    Ok(directory)
}
impl Delivery {
    fn tools(&self) -> Result<super::super::Tools, Error> {
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
            wrix_state::nix_store::admission::request::Plane::Podman => Ok(None),
            wrix_state::nix_store::admission::request::Plane::Host => Err(Error::Invalid),
        }
    }
    /// Validates existing fixed Podman configuration and the exact live readonly projection, without creation or repair.
    pub fn podman_configuration(&self) -> Result<PathBuf, Error> {
        if !self.plane.is_podman_namespace() {
            return Err(Error::Invalid);
        }
        let root = self.key.parent().ok_or(Error::Invalid)?;
        let record = Record::read(root)?;
        if record.plane != self.plane
            || record.pool != self.pool
            || record.ticket != self.ticket
            || record.lease != self.lease
            || self.store != root.join("store")
            || self.known_hosts != root.join("known-hosts")
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
        inspect(root, self.port)
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
