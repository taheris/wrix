//! Native Apple workers use creation-bound names, immutable image receipts and independently observed exit.
mod image;
mod process;
pub use process::Process;
#[cfg(test)]
mod test;
use super::{Environment, Error, Observation, ProjectionMount, runtime_command};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
use wrix_state::execution::{
    Handoff, Removal,
    apple::{Bound, Identity, Recorded},
};

#[derive(Deserialize, Serialize)]
pub struct Snapshot {
    id: String,
    configuration: Value,
    status: Status,
}
#[derive(Deserialize, Serialize)]
struct Status {
    state: State,
    #[serde(rename = "startedDate")]
    started: Option<f64>,
}
#[derive(Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum State {
    Running,
    Stopped,
    Stopping,
    Unknown,
}

pub fn run(runtime: &Command, args: &[&std::ffi::OsStr]) -> Result<Output, Error> {
    let mut output = tempfile::tempfile()?;
    let mut child = runtime_command(runtime, Environment::Fixed)
        .args(runtime.get_args())
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline || output.metadata()?.len() > 1_048_576 {
            child.kill()?;
            child.wait()?;
            return Err(Error::Unknown);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if output.metadata()?.len() > 1_048_576 {
        return Err(Error::Unknown);
    }
    output.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    output.take(1_048_577).read_to_end(&mut stdout)?;
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}
fn required(runtime: &Command, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, Error> {
    let output = run(runtime, args)?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    Ok(output.stdout)
}
fn inspect(runtime: &Command, name: &str) -> Result<Snapshot, Error> {
    parse(
        &required(runtime, &["inspect".as_ref(), name.as_ref()])?,
        name,
    )
}
fn parse(bytes: &[u8], name: &str) -> Result<Snapshot, Error> {
    let mut values: Vec<Snapshot> = serde_json::from_slice(bytes)?;
    if values.len() != 1 {
        return Err(Error::Unknown);
    }
    let value = values.pop().ok_or(Error::Unknown)?;
    if value.id != name || value.configuration["id"] != name {
        return Err(Error::Unknown);
    }
    value.identity()?;
    Ok(value)
}
impl Snapshot {
    fn identity(&self) -> Result<Identity, Error> {
        Ok(Identity::parse(
            &self.id,
            birth(&self.configuration["creationDate"])?,
        )?)
    }
}
fn birth(value: &Value) -> Result<f64, Error> {
    if let Some(native) = value.as_str() {
        let date = chrono::DateTime::parse_from_rfc3339(native).map_err(|_| Error::Unknown)?;
        let epoch = chrono::DateTime::from_timestamp(978_307_200, 0).ok_or(Error::Unknown)?;
        return Ok(date.signed_duration_since(epoch).as_seconds_f64());
    }
    // Older Codable snapshots used seconds since the Foundation epoch.
    value.as_f64().ok_or(Error::Unknown)
}

fn verify_projection(
    config: &Value,
    index: &crate::image::Digest,
    architecture: &str,
    mounts: &[ProjectionMount],
) -> Result<(), Error> {
    if config["image"]["descriptor"]["digest"] != index.as_str()
        || config["platform"]["os"] != "linux"
        || config["platform"]["architecture"] != architecture
        || config["runtimeHandler"] != "container-runtime-linux"
        || config["virtualization"] != false
        || config["ssh"] != false
        || config["rosetta"] != false
        || config["capAdd"] != serde_json::json!(["CAP_NET_ADMIN"])
        || config["publishedPorts"] != serde_json::json!([])
        || config["publishedSockets"] != serde_json::json!([])
        || mounts.len() != 3
    {
        return Err(Error::Delivery);
    }
    if mounts[0].destination != Path::new("/nix/store")
        || mounts[1].destination != Path::new("/run/wrix/nix-pool")
        || mounts[0].source.file_name() != Some("store".as_ref())
        || mounts[1].source.file_name() != Some("worker".as_ref())
        || mounts[0].source.parent() != mounts[1].source.parent()
        || mounts[2].destination != Path::new("/nix/var/nix")
        || mounts[2].source != mounts[1].source.join("state")
    {
        return Err(Error::Delivery);
    }
    let actual = config["mounts"].as_array().ok_or(Error::Delivery)?;
    for value in actual {
        let destination = Path::new(value["destination"].as_str().ok_or(Error::Delivery)?);
        if !destination.is_absolute()
            || destination.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || ["/etc/wrix/apple-nix-bridge", "/run/wrix/apple-nix-bridge"]
                .iter()
                .any(|reserved| {
                    destination.starts_with(reserved)
                        || Path::new(reserved).starts_with(destination)
                })
        {
            return Err(Error::Delivery);
        }
    }
    for expected in mounts {
        let mut matches = actual.iter().filter(|value| {
            value["destination"].as_str().is_some_and(|path| {
                Path::new(path).starts_with(&expected.destination)
                    || expected.destination.starts_with(path)
            })
        });
        let value = matches.next().ok_or(Error::Delivery)?;
        if expected.writable
            || !expected.source.is_absolute()
            || expected.source.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || matches.next().is_some()
            || value["source"].as_str().map(Path::new) != Some(expected.source.as_path())
            || value["destination"].as_str().map(Path::new) != Some(expected.destination.as_path())
            || value["type"] != serde_json::json!({"virtiofs":{}})
            || value["options"] != serde_json::json!(["ro"])
        {
            return Err(Error::Delivery);
        }
    }
    Ok(())
}
fn verify(config: &Value, image: &image::Proof, mounts: &[ProjectionMount]) -> Result<(), Error> {
    verify_projection(config, &image.index, &image.architecture, mounts)?;
    let actual = config["mounts"].as_array().ok_or(Error::Delivery)?;
    let host = wrix_state::host::Host::discover()?;
    for mount in actual {
        let source = Path::new(mount["source"].as_str().ok_or(Error::Delivery)?);
        let managed = mounts.iter().any(|expected| expected.source == source);
        if !managed
            && (source.starts_with(host.roots().state())
                || host.roots().state().starts_with(source))
        {
            return Err(Error::Delivery);
        }
    }
    Ok(())
}

/// Frozen native evidence for an existing worker; it cannot create or start native work.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    identity: Identity,
    configuration: Value,
    image: image::Proof,
    mounts: Vec<ProjectionMount>,
}
impl Binding {
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn matches_delivery(&self, store: &Path, configuration: &Path) -> bool {
        self.mounts.len() == 3
            && self.mounts[0].source == store
            && self.mounts[0].destination == Path::new("/nix/store")
            && !self.mounts[0].writable
            && self.mounts[1].source == configuration
            && self.mounts[1].destination == Path::new("/run/wrix/nix-pool")
            && !self.mounts[1].writable
            && self.mounts[2].source == configuration.join("state")
            && self.mounts[2].destination == Path::new("/nix/var/nix")
            && !self.mounts[2].writable
    }
    /// Rechecks the private generation-fenced binding, original PID, image receipt and entire native config.
    pub fn bridge_command(
        &self,
        runtime: &Command,
        recorded: &Recorded,
        process: &Process,
    ) -> Result<Command, Error> {
        if recorded.identity() != &self.identity || recorded.status()? != Removal::Bound {
            return Err(Error::Delivery);
        }
        process.recheck()?;
        if process.identity() != &self.identity {
            return Err(Error::Delivery);
        }
        let current = inspect(runtime, self.identity.name())?;
        if current.identity()? != self.identity
            || current.configuration != self.configuration
            || current.status.state != State::Running
        {
            return Err(Error::Delivery);
        }
        verify(&current.configuration, &self.image, &self.mounts)?;
        self.image.recheck(runtime)?;
        let mut command = runtime_command(runtime, Environment::Fixed);
        command
            .args(["exec", "--interactive", self.identity.name()])
            .arg(&self.image.executable)
            .arg("serve");
        Ok(command)
    }
}

/// Constructible only after the exact native configuration and original one-shot reservation are bound.
pub struct Worker {
    bound: Bound,
    runtime: Command,
    configuration: Value,
    image: image::Proof,
    mounts: Vec<ProjectionMount>,
}
impl Worker {
    pub const fn ticket(&self) -> &wrix_state::execution::Ticket {
        self.bound.ticket()
    }
    pub const fn identity(&self) -> &Identity {
        self.bound.identity()
    }
    fn recheck(&self) -> Result<Snapshot, Error> {
        self.bound.recheck()?;
        let current = inspect(&self.runtime, self.identity().name())?;
        if current.identity()? != *self.identity() || current.configuration != self.configuration {
            return Err(Error::Delivery);
        }
        verify(&current.configuration, &self.image, &self.mounts)?;
        self.image.recheck(&self.runtime)?;
        Ok(current)
    }
    pub fn binding(&self) -> Result<Binding, Error> {
        self.recheck()?;
        Ok(Binding {
            identity: self.identity().clone(),
            configuration: self.configuration.clone(),
            image: self.image.clone(),
            mounts: self.mounts.clone(),
        })
    }
    /// Only this checked worker's immutable image closure helper can receive untrusted broker bytes.
    pub fn runtime_process(&self) -> Result<Process, Error> {
        if self.recheck()?.status.state != State::Running {
            return Err(Error::Unknown);
        }
        Process::observe(self.identity())
    }
    pub fn bridge_command(&self, runtime: &Command) -> Result<Command, Error> {
        if runtime.get_program() != self.runtime.get_program()
            || runtime.get_current_dir() != self.runtime.get_current_dir()
            || !runtime.get_envs().eq(self.runtime.get_envs())
        {
            return Err(Error::Delivery);
        }
        if self.recheck()?.status.state != State::Running {
            return Err(Error::Unknown);
        }
        let mut command = runtime_command(runtime, Environment::Fixed);
        command
            .args(["exec", "--interactive", self.identity().name()])
            .arg(&self.image.executable)
            .arg("serve");
        Ok(command)
    }
}
pub struct Prepared {
    worker: Worker,
    interactive: bool,
}
impl Prepared {
    pub fn create(
        handoff: Handoff,
        ticket: &wrix_state::execution::Ticket,
        command: &Command,
        interactive: bool,
        expected: &crate::image::Digest,
        seed: &Path,
        mounts: &[ProjectionMount],
    ) -> Result<Self, Error> {
        let mut args = command.get_args();
        if args.next() != Some("run".as_ref()) || args.next() != Some("--rm".as_ref()) {
            return Err(Error::Delivery);
        }
        let args: Vec<_> = args.collect();
        let boundary = args
            .iter()
            .position(|arg| **arg == *std::ffi::OsStr::new("--"))
            .ok_or(Error::Delivery)?;
        let reference = args.get(boundary + 1).ok_or(Error::Delivery)?;
        if args[..boundary]
            .iter()
            .any(|arg| arg.as_encoded_bytes().starts_with(b"--name"))
        {
            return Err(Error::Delivery);
        }
        let runtime = runtime_command(command, Environment::Fixed);
        let image = image::Proof::inspect(&runtime, reference, expected, seed)?;
        let name = format!("wrix-worker-{}", &ticket.as_str()[..40]);
        let mut create = runtime_command(&runtime, Environment::Fixed);
        create
            .args(["create", "--name", &name])
            .args(&args[..boundary])
            .arg("--")
            .arg(image.reference.as_str())
            .args(&args[boundary + 2..]);
        let output = required(&create, &[])?;
        if std::str::from_utf8(&output)
            .map_err(|_| Error::Unknown)?
            .trim()
            != name
        {
            return Err(Error::Unknown);
        }
        let native = inspect(&runtime, &name)?;
        let bound = handoff.bind_apple(native.identity()?)?;
        verify(&native.configuration, &image, mounts)?;
        if native.status.state != State::Stopped || native.status.started.is_some() {
            return Err(Error::Unknown);
        }
        Ok(Self {
            worker: Worker {
                bound,
                runtime,
                configuration: native.configuration,
                image,
                mounts: mounts.to_vec(),
            },
            interactive,
        })
    }
    pub fn spawn(self, session: &dyn crate::nix_store::Session) -> Result<Running, Error> {
        let worker = self.worker;
        if worker.recheck()?.status.state != State::Stopped {
            return Err(Error::Unknown);
        }
        let mut start = runtime_command(&worker.runtime, Environment::Fixed);
        start.args(["start", "--attach"]);
        if self.interactive {
            start.arg("--interactive");
        }
        let child = start
            .arg(worker.identity().name())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while worker.recheck()?.status.state != State::Running {
            if Instant::now() >= deadline {
                return Err(Error::Unknown);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let process = worker.runtime_process()?;
        let bridge = session
            .start_apple_bridge(&worker)
            .map_err(std::io::Error::other)?;
        Ok(Running {
            worker,
            process,
            child,
            _bridge: bridge,
        })
    }
}
pub struct Running {
    worker: Worker,
    process: Process,
    child: Child,
    _bridge: Box<dyn crate::nix_store::Transport>,
}
impl Running {
    pub const fn child(&mut self) -> &mut Child {
        &mut self.child
    }
    pub fn reconcile(&self) -> Result<Observation, Error> {
        reconcile(
            &self.worker.bound.recorded()?,
            &self.worker.runtime,
            &self.process,
        )
    }
    pub fn wait_for_retirement(&self, budget: Duration) -> Result<Observation, Error> {
        let deadline = Instant::now() + budget;
        loop {
            let observation = self.reconcile()?;
            if observation == Observation::Retired || Instant::now() >= deadline {
                return Ok(observation);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
fn stopped(native: &Snapshot, identity: &Identity) -> Result<bool, Error> {
    if native.identity()? != *identity {
        return Err(Error::Unknown);
    }
    Ok(native.status.state == State::Stopped
        && native
            .status
            .started
            .is_some_and(|date| date.is_finite() && date > 0.0))
}
/// Native stopped-after-start, durable removal intent and independently observed absence are all required.
pub fn reconcile(
    record: &Recorded,
    runtime: &Command,
    process: &Process,
) -> Result<Observation, Error> {
    if process.identity() != record.identity() {
        return Err(Error::Unknown);
    }
    let status = record.status()?;
    if status == Removal::Retired {
        return Ok(Observation::Retired);
    }
    if status == Removal::Bound {
        let native = inspect(runtime, record.identity().name())?;
        if !stopped(&native, record.identity())? {
            return Ok(Observation::Protected);
        }
        if !process.gone()? {
            return Ok(Observation::Protected);
        }
        record.removing()?;
        required(
            runtime,
            &["delete".as_ref(), record.identity().name().as_ref()],
        )?;
    }
    let bytes = required(
        runtime,
        &[
            "list".as_ref(),
            "--all".as_ref(),
            "--format".as_ref(),
            "json".as_ref(),
        ],
    )?;
    let objects: Vec<Snapshot> = serde_json::from_slice(&bytes)?;
    if objects
        .iter()
        .any(|object| object.id == record.identity().name())
    {
        return Err(Error::Unknown);
    }
    if !process.gone()? {
        return Ok(Observation::Protected);
    }
    record.retire()?;
    Ok(Observation::Retired)
}
