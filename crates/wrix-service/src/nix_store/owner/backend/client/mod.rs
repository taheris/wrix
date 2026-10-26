//! Execution-bound client and read-only projection ownership. Drop/disconnect never releases either.
mod mount;
mod path;
pub(crate) mod supervisor;
#[cfg(test)]
mod test;
pub mod worker;
use super::{
    COMMAND_BUDGET, Error, Owner, Program, SshPath, check_directory, known_hosts_option,
    private_file, public_key, random, ssh_path_option, success,
};
use crate::nix_store::native::{self, lease};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    num::NonZeroU16,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::DirBuilderExt,
    },
    path::{Path, PathBuf},
};
use wrix_core::nix_store::pool::Key;
use wrix_state::{
    execution::{Terminal, Ticket},
    nix_store::admission::request::{Accepted, Plane, Purpose},
};

/// A host-private delivery description; it is not workspace authority or native exit evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    #[serde(default, skip_serializing_if = "Plane::is_host")]
    plane: Plane,
    pool: Key,
    ticket: Ticket,
    lease: lease::Id,
    #[serde(with = "path")]
    store: PathBuf,
    #[serde(with = "path")]
    key: PathBuf,
    #[serde(with = "path")]
    known_hosts: PathBuf,
    port: NonZeroU16,
}
impl Delivery {
    pub const fn pool(&self) -> &Key {
        &self.pool
    }
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }
    /// Exact runtime bind source, not a host traversal root; links resolve inside the worker namespace.
    pub fn store(&self) -> &Path {
        &self.store
    }
    pub fn key(&self) -> &Path {
        &self.key
    }
    pub fn known_hosts(&self) -> &Path {
        &self.known_hosts
    }
    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum State {
    Registering,
    Attaching,
    Attached { mount: mount::Identity },
    Detaching { mount: mount::Identity },
    Detached,
    Released,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    #[serde(default, skip_serializing_if = "Plane::is_host")]
    plane: Plane,
    pool: Key,
    namespace: crate::control::Namespace,
    generation: Vec<u8>,
    ticket: Ticket,
    lease: lease::Id,
    state: State,
}
impl Record {
    fn read(root: &Path) -> Result<Self, Error> {
        check_directory(root)?;
        let file = private_file(root, "client.json", false)?;
        if file.metadata()?.len() > 8192 {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(8193).read_to_end(&mut bytes)?;
        let value: Self = serde_json::from_slice(&bytes)?;
        if value.version != 1 || value.generation.len() != 32 {
            return Err(Error::Invalid);
        }
        Ok(value)
    }
    fn publish(&self, root: &Path) -> Result<(), Error> {
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.as_file().sync_all()?;
        file.persist(root.join("client.json"))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
}

fn fuse_option(value: &std::ffi::OsStr) -> OsString {
    let mut bytes = Vec::new();
    for byte in value.as_bytes() {
        if matches!(byte, b',' | b'\\') {
            bytes.push(b'\\');
        }
        bytes.push(*byte);
    }
    OsString::from_vec(bytes)
}

pub(super) fn protected(
    control: &Path,
    pool: &Key,
    indexed: &BTreeSet<Ticket>,
) -> Result<bool, Error> {
    Ok(retained(control, pool, indexed)? != 0)
}

pub(super) fn retained(
    control: &Path,
    pool: &Key,
    indexed: &BTreeSet<Ticket>,
) -> Result<usize, Error> {
    let root = control.join("executions");
    match fs::symlink_metadata(&root) {
        Ok(_) => check_directory(&root)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound && indexed.is_empty() => {
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    }
    let mut retained = 0;
    let mut seen = BTreeSet::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let ticket = Ticket::parse(entry.file_name().to_str().ok_or(Error::Invalid)?)?;
        let record = Record::read(&entry.path())?;
        if record.ticket != ticket || &record.pool != pool {
            return Err(Error::Invalid);
        }
        if !matches!(record.state, State::Released) {
            if !indexed.contains(&ticket) {
                return Err(Error::Invalid);
            }
            retained += 1;
        }
        seen.insert(ticket);
    }
    if !indexed.is_subset(&seen) {
        return Err(Error::Retained);
    }
    Ok(retained)
}

impl Owner {
    /// A verified original execution reservation, not a path/ticket description, admits these fixed roles.
    pub fn admit_execution(&mut self, issued: &Accepted) -> Result<Delivery, Error> {
        issued.recheck()?;
        if issued.pool() != self.pool() {
            return Err(Error::Invalid);
        }
        let plane = match issued.purpose() {
            Purpose::Worker { plane, .. } => *plane,
            _ => Plane::Host,
        };
        let ticket = match issued.purpose() {
            Purpose::Execution { ticket } | Purpose::Worker { ticket, .. } => ticket,
            Purpose::Preparation => return Err(Error::Invalid),
        };
        if !matches!(self.record.state, super::State::Serving { .. }) {
            return Err(Error::Retained);
        }
        self.connection()?
            .target
            .attempt
            .software
            .tools
            .view_check(plane)?;
        if let Purpose::Worker { image: binding, .. } = issued.purpose() {
            let digest = wrix_sandbox::image::Digest::parse(binding.digest().as_str())
                .map_err(|_| Error::Invalid)?;
            let image =
                super::seed::Image::inspect(self.pool().system, &digest, binding.metadata())?;
            if !matches!(
                self.rpc(
                    self.connection()?,
                    &native::Operation::InspectImage {
                        image: image.id().clone(),
                        paths: image.closure_paths().to_vec()
                    },
                    Some(COMMAND_BUDGET)
                )?,
                native::Reply::Image {
                    status: native::ImageStatus::Pinned
                }
            ) {
                return Err(Error::Retained);
            }
        }
        protected(
            &self.prepared.reservation.paths.control,
            self.pool(),
            &self.record.executions,
        )?;
        let mut released = Vec::new();
        for ticket in &self.record.executions {
            if matches!(
                Record::read(
                    &self
                        .prepared
                        .reservation
                        .paths
                        .control
                        .join("executions")
                        .join(ticket.as_str())
                )?
                .state,
                State::Released
            ) {
                released.push(ticket.clone());
            }
        }
        if !released.is_empty() {
            for ticket in released {
                self.record.executions.remove(&ticket);
            }
            self.record
                .publish(&self.prepared.reservation.paths.control)?;
        }
        self.inspect()?;
        let connection = self.connection()?.clone();
        let tools = &connection.target.attempt.software.tools;
        let parent = self.prepared.reservation.paths.control.join("executions");
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => File::open(&self.prepared.reservation.paths.control)?.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => check_directory(&parent)?,
            Err(error) => return Err(error.into()),
        }
        let root = parent.join(ticket.as_str());
        ssh_path_option(SshPath::Identity, &root.join("key"))?;
        known_hosts_option(&root.join("known-hosts"))?;
        let mut record = match fs::symlink_metadata(&root) {
            Ok(_) => {
                if !self.record.executions.contains(ticket) {
                    return Err(Error::Retained);
                }
                let record = Record::read(&root)?;
                if record.plane != plane
                    || record.pool != *issued.pool()
                    || record.ticket != *ticket
                    || record.namespace.as_str() != issued.namespace().as_str()
                    || record.generation != issued.generation().as_bytes()
                {
                    return Err(Error::Invalid);
                }
                let State::Attached { mount: expected } = &record.state else {
                    return Err(Error::Retained);
                };
                if mount::observe(record.plane, tools, &root.join("store"), &record.lease)?.as_ref()
                    != Some(expected)
                {
                    return Err(Error::Retained);
                }
                if !mount::readable(record.plane, tools, &root.join("store"))? {
                    return Err(Error::Retained);
                }
                private_file(&root, "key", false)?;
                private_file(&root, "known-hosts", false)?;
                return Ok(Delivery {
                    plane: record.plane,
                    pool: record.pool,
                    ticket: record.ticket,
                    lease: record.lease,
                    store: root.join("store"),
                    key: root.join("key"),
                    known_hosts: root.join("known-hosts"),
                    port: connection.port,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if self.record.executions.contains(ticket) {
                    return Err(Error::Retained);
                }
                if self.record.executions.len() >= 64 {
                    return Err(Error::Busy);
                }
                self.record.executions.insert(ticket.clone());
                self.record
                    .publish(&self.prepared.reservation.paths.control)?;
                fs::DirBuilder::new().mode(0o700).create(&root)?;
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(root.join("store"))?;
                let mut keygen = tools.command(Program::Keygen);
                keygen
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
                    .arg(root.join("key"));
                success(tools.run(keygen, None, Some(COMMAND_BUDGET))?)?;
                private_file(&root, "key", false)?.sync_all()?;
                let source = private_file(
                    &self.prepared.reservation.paths.control,
                    "known-hosts",
                    false,
                )?;
                if source.metadata()?.len() > 8192 {
                    return Err(Error::Invalid);
                }
                let mut destination = private_file(&root, "known-hosts", true)?;
                io::copy(&mut source.take(8193), &mut destination)?;
                if destination.metadata()?.len() > 8192 {
                    return Err(Error::Invalid);
                }
                destination.sync_all()?;
                if matches!(issued.purpose(), Purpose::Worker { .. }) {
                    worker::create(&root, connection.port)?;
                }
                let record = Record {
                    version: 1,
                    plane,
                    pool: issued.pool().clone(),
                    namespace: crate::control::Namespace::from(issued.namespace()),
                    generation: issued.generation().as_bytes().to_vec(),
                    ticket: ticket.clone(),
                    lease: random()?,
                    state: State::Registering,
                };
                record.publish(&root)?;
                File::open(&parent)?.sync_all()?;
                record
            }
            Err(error) => return Err(error.into()),
        };
        let key = public_key(&root.join("key.pub"))?;
        if !matches!(
            self.rpc(
                &connection,
                &native::Operation::Register {
                    id: record.lease.clone(),
                    key
                },
                Some(COMMAND_BUDGET)
            )?,
            native::Reply::Applied
        ) {
            return Err(Error::Retained);
        }
        record.state = State::Attaching;
        record.publish(&root)?;
        let mut command = tools.view_command(plane, Program::Sshfs);
        command.arg("-p").arg(connection.port.to_string());
        let ssh = tools.command(Program::Ssh).get_program().to_owned();
        let mut ssh_command = OsString::from("ssh_command=");
        ssh_command.push(ssh);
        ssh_command.push(" -F /dev/null");
        command.arg("-o").arg(ssh_command);
        for option in [
            "ro",
            "nodev",
            "nosuid",
            "cache=no",
            "attr_timeout=60",
            "entry_timeout=60",
            "negative_timeout=0",
            "BatchMode=yes",
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "StrictHostKeyChecking=yes",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
        ] {
            command.arg("-o").arg(option);
        }
        if plane == Plane::Podman {
            // Installed inodes cannot change while this execution excludes native GC.
            command
                .arg("-o")
                .arg("no_contain_symlinks")
                .arg("-o")
                .arg("kernel_cache");
        }
        command.arg("-o").arg(fuse_option(&ssh_path_option(
            SshPath::Identity,
            &root.join("key"),
        )?));
        command
            .arg("-o")
            .arg(fuse_option(&known_hosts_option(&root.join("known-hosts"))?));
        command
            .arg("-o")
            .arg(format!("fsname=wrix-nix-{}", record.lease.as_str()));
        command
            .arg("nix-export@127.0.0.1:/nix/store")
            .arg(root.join("store"));
        success(tools.run(command, None, Some(COMMAND_BUDGET))?)?;
        let mounted = mount::observe(record.plane, tools, &root.join("store"), &record.lease)?
            .ok_or(Error::Retained)?;
        if !mount::readable(record.plane, tools, &root.join("store"))? {
            return Err(Error::Retained);
        }
        record.state = State::Attached { mount: mounted };
        record.publish(&root)?;
        Ok(Delivery {
            plane: record.plane,
            pool: record.pool,
            ticket: record.ticket,
            lease: record.lease,
            store: root.join("store"),
            key: root.join("key"),
            known_hosts: root.join("known-hosts"),
            port: connection.port,
        })
    }

    /// Terminal native evidence precedes graceful unmount and authenticated native-work drain; never force either.
    pub fn release_execution(&mut self, proof: &Terminal) -> Result<(), Error> {
        let root = self
            .prepared
            .reservation
            .paths
            .control
            .join("executions")
            .join(proof.ticket().as_str());
        let mut record = Record::read(&root)?;
        if record.pool != *self.pool()
            || record.ticket != *proof.ticket()
            || record.namespace.as_str() != proof.namespace().as_str()
            || record.generation != proof.generation().as_bytes()
        {
            return Err(Error::Invalid);
        }
        if !matches!(record.state, State::Released)
            && !self.record.executions.contains(&record.ticket)
        {
            return Err(Error::Invalid);
        }
        let connection = self.connection()?.clone();
        let tools = &connection.target.attempt.software.tools;
        match &record.state {
            State::Attached { mount: expected } | State::Detaching { mount: expected } => {
                let expected = expected.clone();
                let observed =
                    mount::observe(record.plane, tools, &root.join("store"), &record.lease)?;
                if observed.as_ref().is_some_and(|actual| actual != &expected)
                    || (observed.is_none() && !matches!(record.state, State::Detaching { .. }))
                {
                    return Err(Error::Retained);
                }
                record.state = State::Detaching {
                    mount: expected.clone(),
                };
                record.publish(&root)?;
                if observed.is_some() {
                    success(tools.unmount(record.plane, &root.join("store"))?)?;
                }
                if mount::observe(record.plane, tools, &root.join("store"), &record.lease)?
                    .is_some()
                {
                    return Err(Error::Retained);
                }
                if record.plane == Plane::Host
                    && let Some(replica) =
                        mount::observe(Plane::Podman, tools, &root.join("store"), &record.lease)?
                {
                    if !replica.same_filesystem(&expected) {
                        return Err(Error::Retained);
                    }
                    success(tools.unmount(Plane::Podman, &root.join("store"))?)?;
                    if mount::observe(Plane::Podman, tools, &root.join("store"), &record.lease)?
                        .is_some()
                    {
                        return Err(Error::Retained);
                    }
                }
                record.state = State::Detached;
                record.publish(&root)?;
            }
            State::Detached => (),
            State::Released => {
                if self.record.executions.remove(&record.ticket) {
                    self.record
                        .publish(&self.prepared.reservation.paths.control)?;
                }
                return Ok(());
            }
            _ => return Err(Error::Retained),
        }
        match self.rpc(
            &connection,
            &native::Operation::Release {
                id: record.lease.clone(),
            },
            Some(COMMAND_BUDGET),
        )? {
            native::Reply::Applied => {
                record.state = State::Released;
                record.publish(&root)?;
                self.record.executions.remove(&record.ticket);
                self.record
                    .publish(&self.prepared.reservation.paths.control)?;
                Ok(())
            }
            native::Reply::Busy => Err(Error::Busy),
            _ => Err(Error::Retained),
        }
    }
}
