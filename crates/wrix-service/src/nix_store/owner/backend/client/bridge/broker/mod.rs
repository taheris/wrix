//! A private one-shot launchd unit owns transport independently of every observer and controller.
#[cfg(test)]
mod test;
use super::super::{Delivery, Record as Client, State, check_directory, private_file};
use super::{Bridge, Error};
use crate::nix_store::{
    native::lease,
    owner::backend::{Program, tools::Tools},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};
use wrix_sandbox::execution::apple::{Binding, Process};
use wrix_state::{
    execution::{Removal, apple::Recorded},
    host::Host,
    nix_store::admission::request::Plane,
};

const RECORD: &str = "broker.json";
const PLIST: &str = "broker.plist";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    epoch: lease::Id,
    delivery: Delivery,
    namespace: crate::control::Namespace,
    generation: Vec<u8>,
    tools: Tools,
    environment: BTreeMap<String, String>,
    worker: Binding,
    process: Process,
    key: Vec<u8>,
    known_hosts: Vec<u8>,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum Status {
    Ready { epoch: lease::Id, pid: u32 },
    Failed { epoch: lease::Id, pid: u32 },
    Released { epoch: lease::Id, pid: u32 },
}
fn read<T: serde::de::DeserializeOwned>(root: &Path, name: &str) -> Result<T, Error> {
    let file = private_file(root, name, false)?;
    if file.metadata()?.len() > 1_048_576 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn publish<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<(), Error> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(root.join(name)).map_err(|error| error.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}
fn create(root: &Path, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut file = private_file(root, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}
fn hash(root: &Path, name: &str) -> Result<Vec<u8>, Error> {
    let file = private_file(root, name, false)?;
    if file.metadata()?.len() > 16_384 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes)?;
    Ok(Sha256::digest(bytes).to_vec())
}
fn ancestry(host: &Host, root: &Path) -> Result<(), Error> {
    ancestry_under(host.roots().state(), root)
}
fn ancestry_under(state: &Path, root: &Path) -> Result<(), Error> {
    let tail = root.strip_prefix(state).map_err(|_| Error::Invalid)?;
    if tail.components().count() != 6
        || tail
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Error::Invalid);
    }
    let mut path = state.to_owned();
    check_directory(&path)?;
    for part in tail.components() {
        path.push(part);
        check_directory(&path)?;
    }
    Ok(())
}
fn environment(command: &Command) -> Result<BTreeMap<String, String>, Error> {
    command
        .get_envs()
        .map(|(key, value)| {
            Ok((
                key.to_str().ok_or(Error::Invalid)?.to_owned(),
                value
                    .and_then(std::ffi::OsStr::to_str)
                    .ok_or(Error::Invalid)?
                    .to_owned(),
            ))
        })
        .collect()
}
impl Record {
    fn root(&self, host: &Host) -> PathBuf {
        host.roots()
            .state()
            .join("control/nix")
            .join(self.delivery.pool.repository.to_string())
            .join(self.delivery.pool.system.as_str())
            .join("executions")
            .join(self.delivery.ticket.as_str())
    }
    fn check(&self, host: &Host, root: &Path, client: &Client) -> Result<(), Error> {
        if self.root(host) != root
            || (!matches!(client.state, State::Released) && self.tools != self.delivery.tools()?)
        {
            return Err(Error::Retained);
        }
        self.binding(root, client)
    }
    fn binding(&self, root: &Path, client: &Client) -> Result<(), Error> {
        if self.version != 1
            || self.delivery.plane != Plane::Apple
            || client.plane != Plane::Apple
            || self.delivery.pool != client.pool
            || self.delivery.ticket != client.ticket
            || self.delivery.lease != client.lease
            || self.namespace != client.namespace
            || self.generation != client.generation
            || self.generation.len() != 32
            || client.port != Some(self.delivery.port)
            || client.broker_epoch.as_ref() != Some(&self.epoch)
            || client.native_process.as_ref() != Some(&self.process)
            || self.worker.identity() != self.process.identity()
            || !self
                .worker
                .matches_delivery(&self.delivery.store, &root.join("worker"))
            || self.delivery.key != root.join("key")
            || self.delivery.known_hosts != root.join("known-hosts")
            || self.delivery.store != root.join("store")
            || self.key != hash(root, "key")?
            || self.known_hosts != hash(root, "known-hosts")?
        {
            return Err(Error::Retained);
        }
        Ok(())
    }
    fn recorded(&self, host: Arc<Host>) -> Result<Recorded, Error> {
        Recorded::load(
            host,
            &wrix_core::path::WorkspaceHash::parse(self.namespace.as_str())
                .map_err(|_| Error::Invalid)?,
            &self.delivery.ticket,
            &wrix_state::store::Generation::parse(&self.generation)?,
        )?
        .ok_or(Error::Retained)
    }
    fn runtime(&self) -> Command {
        let mut command = Command::new(self.tools.command(Program::Container).get_program());
        command.current_dir("/").env_clear().envs(&self.environment);
        command
    }
    fn executable(&self) -> PathBuf {
        Path::new(self.tools.command(Program::Container).get_program())
            .with_file_name("wrix-nix-worker-broker")
    }
    fn label(&self) -> String {
        format!("org.wrix.nix-worker.{}", self.epoch.as_str())
    }
    fn unit(&self) -> String {
        format!(
            "gui/{}/{}",
            rustix::process::getuid().as_raw(),
            self.label()
        )
    }
    fn plist(&self, root: &Path) -> Result<String, Error> {
        let mut environment = String::new();
        for (key, value) in &self.environment {
            use std::fmt::Write as _;
            write!(
                environment,
                "<key>{}</key><string>{}</string>",
                xml(key)?,
                xml(value)?
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(format!(
            "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--control</string><string>{}</string><string>--epoch</string><string>{}</string></array><key>WorkingDirectory</key><string>/</string><key>EnvironmentVariables</key><dict>{environment}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>",
            self.label(),
            xml(self.executable().to_str().ok_or(Error::Invalid)?)?,
            xml(root.to_str().ok_or(Error::Invalid)?)?,
            self.epoch.as_str()
        ))
    }
    fn observe(&self, root: &Path) -> Result<(), Error> {
        let Status::Ready { epoch, pid } = read(root, "broker-status.json")? else {
            return Err(Error::Retained);
        };
        if epoch != self.epoch || pid == 0 {
            return Err(Error::Retained);
        }
        let expected = self.plist(root)?;
        let mut bytes = String::new();
        private_file(root, PLIST, false)?
            .take(65_537)
            .read_to_string(&mut bytes)?;
        if bytes != expected {
            return Err(Error::Retained);
        }
        let output = launchctl(&["print".as_ref(), self.unit().as_ref()])?;
        if !output.status.success() {
            return Err(Error::Retained);
        }
        native(&output.stdout, root, self.executable().as_os_str(), pid)
    }
}
fn xml(value: &str) -> Result<String, Error> {
    if value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}
fn native(bytes: &[u8], root: &Path, program: &std::ffi::OsStr, pid: u32) -> Result<(), Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?;
    for (prefix, expected) in [
        ("path = ", root.join(PLIST).display().to_string()),
        ("program = ", Path::new(program).display().to_string()),
        ("pid = ", pid.to_string()),
    ] {
        let values: Vec<_> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix(prefix))
            .collect();
        if values != [expected.as_str()] {
            return Err(Error::Retained);
        }
    }
    Ok(())
}
fn launchctl(args: &[&std::ffi::OsStr]) -> Result<std::process::Output, Error> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Unsupported);
    }
    let mut command = Command::new("/bin/launchctl");
    command.env_clear().current_dir("/");
    wrix_sandbox::execution::apple::run(&command, args)
        .map_err(|error| std::io::Error::other(error).into())
}
fn lock(root: &Path) -> Result<File, Error> {
    let file = match private_file(root, "broker-start.lock", true) {
        Ok(file) => file,
        Err(crate::nix_store::owner::Error::Io(error))
            if error.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            private_file(root, "broker-start.lock", false)?
        }
        Err(error) => return Err(error.into()),
    };
    file.try_lock_exclusive().map_err(|_| Error::Busy)?;
    Ok(file)
}

pub(in crate::nix_store::owner::backend::client) fn start(
    delivery: &Delivery,
    worker: &wrix_sandbox::execution::apple::Worker,
) -> Result<Bridge, Error> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Unsupported);
    }
    if worker.ticket() != delivery.ticket() {
        return Err(Error::Invalid);
    }
    delivery.apple_configuration()?;
    let host = Arc::new(Host::discover().map_err(std::io::Error::other)?);
    let root = delivery.key.parent().ok_or(Error::Invalid)?;
    ancestry(&host, root)?;
    let _lock = lock(root)?;
    let mut client = Client::read(root)?;
    if let Some(record) = existing(root, &client)? {
        record.check(&host, root, &client)?;
        if &record.delivery != delivery
            || record.worker.identity() != worker.identity()
            || serde_json::to_vec(&record.worker)?
                != serde_json::to_vec(&worker.binding().map_err(std::io::Error::other)?)?
        {
            return Err(Error::Retained);
        }
        record.process.recheck().map_err(std::io::Error::other)?;
        record.observe(root)?;
        return Ok(Bridge {
            _epoch: record.epoch,
        });
    }
    let tools = delivery.tools()?;
    let process = worker.runtime_process().map_err(std::io::Error::other)?;
    let record = Record {
        version: 1,
        epoch: super::super::random()?,
        delivery: delivery.clone(),
        namespace: client.namespace.clone(),
        generation: client.generation.clone(),
        environment: environment(&tools.command(Program::Container))?,
        tools,
        worker: worker.binding().map_err(std::io::Error::other)?,
        process: process.clone(),
        key: hash(root, "key")?,
        known_hosts: hash(root, "known-hosts")?,
    };
    let roots = root.join("broker-roots");
    fs::DirBuilder::new().mode(0o700).create(&roots)?;
    record.tools.pin(&roots)?;
    client.native_process = Some(process);
    client.broker_epoch = Some(record.epoch.clone());
    client.publish(root)?;
    create(root, RECORD, &serde_json::to_vec(&record)?)?;
    create(root, PLIST, record.plist(root)?.as_bytes())?;
    let loaded = launchctl(&["print".as_ref(), record.unit().as_ref()])?;
    if loaded.status.code() != Some(113) || !loaded.stdout.is_empty() {
        return Err(Error::Retained);
    }
    let output = launchctl(&[
        "bootstrap".as_ref(),
        format!("gui/{}", rustix::process::getuid().as_raw()).as_ref(),
        root.join(PLIST).as_os_str(),
    ])?;
    if !output.status.success() {
        return Err(Error::Retained);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match record.observe(root) {
            Ok(()) => {
                return Ok(Bridge {
                    _epoch: record.epoch,
                });
            }
            Err(error) if Instant::now() < deadline => {
                tracing::debug!(operation = "apple_nix_broker_start", %error, "waiting for original broker acquisition only");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

fn existing(root: &Path, client: &Client) -> Result<Option<Record>, Error> {
    if let Some(epoch) = &client.broker_epoch {
        let record: Record = read(root, RECORD)?;
        record.binding(root, client)?;
        if &record.epoch != epoch {
            return Err(Error::Retained);
        }
        return Ok(Some(record));
    }
    if client.native_process.is_some()
        || root.join(RECORD).try_exists()?
        || root.join(PLIST).try_exists()?
        || root.join("broker-consumed.json").try_exists()?
        || root.join("broker-status.json").try_exists()?
    {
        return Err(Error::Retained);
    }
    Ok(None)
}

/// Observation only: absent, failed or replaced brokers are retained, never bootstrapped or replayed.
pub(in crate::nix_store::owner::backend::client) fn reconcile(
    host: &Arc<Host>,
    root: &Path,
    client: &Client,
) -> Result<(), Error> {
    ancestry(host, root)?;
    let record: Record = read(root, RECORD)?;
    record.check(host, root, client)?;
    let recorded = record.recorded(host.clone())?;
    if recorded.identity() != record.worker.identity() {
        return Err(Error::Retained);
    }
    if recorded.status()? == Removal::Bound {
        record
            .worker
            .bridge_command(&record.runtime(), &recorded, &record.process)
            .map_err(std::io::Error::other)?;
        record.observe(root)?;
    }
    Ok(())
}

fn arguments(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(PathBuf, lease::Id), Error> {
    let args: Vec<_> = args.collect();
    if args.len() != 4 || args[0] != "--control" || args[2] != "--epoch" {
        return Err(Error::Invalid);
    }
    let epoch = args[3]
        .to_str()
        .ok_or(Error::Invalid)?
        .to_owned()
        .try_into()?;
    Ok((PathBuf::from(&args[1]), epoch))
}

/// Private helper entry point. There is no forwarding target or executable selector.
pub fn main() -> Result<(), Error> {
    let host = Arc::new(Host::discover().map_err(std::io::Error::other)?);
    if !cfg!(target_os = "macos") {
        return Err(Error::Unsupported);
    }
    let (root, epoch) = arguments(std::env::args_os().skip(1))?;
    ancestry(&host, &root)?;
    let record: Record = read(&root, RECORD)?;
    record.check(&host, &root, &Client::read(&root)?)?;
    if record.epoch != epoch
        || std::env::current_exe()?.canonicalize()? != record.executable().canonicalize()?
        || environment(&record.tools.command(Program::Container))? != record.environment
    {
        return Err(Error::Retained);
    }
    create(&root, "broker-consumed.json", &serde_json::to_vec(&epoch)?)?;
    let result = serve(&host, &root, &record);
    if let Err(error) = &result {
        if let Err(write) = publish(
            &root,
            "broker-status.json",
            &Status::Failed {
                epoch,
                pid: std::process::id(),
            },
        ) {
            tracing::warn!(operation = "apple_nix_broker_failure", %write, "cannot publish failed observer status; protection retained");
        }
        tracing::error!(operation = "apple_nix_broker", %error, "transport failed; no worker exit or completion inferred");
    }
    result
}
fn released(client: &Client, removal: Removal) -> Result<bool, Error> {
    if matches!(client.state, State::Released) {
        if removal != Removal::Retired {
            return Err(Error::Retained);
        }
        return Ok(true);
    }
    Ok(false)
}
fn completion(
    host: &Host,
    root: &Path,
    record: &Record,
    recorded: &Recorded,
) -> Result<bool, Error> {
    let client = Client::read(root)?;
    record.check(host, root, &client)?;
    released(&client, recorded.status()?)
}
fn serve(host: &Arc<Host>, root: &Path, record: &Record) -> Result<(), Error> {
    record.delivery.apple_configuration()?;
    let recorded = record.recorded(host.clone())?;
    let command = record
        .worker
        .bridge_command(&record.runtime(), &recorded, &record.process)
        .map_err(std::io::Error::other)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let (ready, mut startup) = tokio::sync::oneshot::channel();
        let transport = super::run(command, record.delivery.port, receiver, ready);
        tokio::pin!(transport);
        tokio::select! {
            result = &mut transport => return result.and(Err(Error::Retained)),
            result = &mut startup => { result.map_err(|_| Error::Retained)?; }
        }
        publish(root, "broker-status.json", &Status::Ready { epoch: record.epoch.clone(), pid: std::process::id() })?;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                result = &mut transport => return result.and(Err(Error::Retained)),
                _ = interval.tick() => {
                    let completed = match completion(host, root, record, &recorded) {
                        Ok(completed) => completed,
                        Err(error) => {
                            tracing::warn!(operation = "apple_nix_broker_observe", %error, "lifecycle observation is uncertain; original fixed transport and protection retained");
                            false
                        }
                    };
                    if completed {
                        stop.send(()).map_err(|()| Error::Retained)?;
                        transport.await?;
                        publish(root, "broker-status.json", &Status::Released { epoch: record.epoch.clone(), pid: std::process::id() })?;
                        return Ok(());
                    }
                }
            }
        }
    })
}
