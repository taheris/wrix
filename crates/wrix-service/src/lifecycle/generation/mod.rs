//! Host-private native generation intent. Only verified fixed role endpoints become controller routes.

use super::{Error, Result, Runtime, RuntimeKind, RuntimeStatus, owner};
use crate::{
    control::supervision::{Lifecycle, Native, Package, Platform},
    deployment::{Generation, Id},
    gateway::{Secret, management::Client},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod test;
use std::{
    fs::{self, File},
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener},
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wrix_core::path::ContainerName;
use wrix_state::host::Host;

const PLAN: &str = "native-plan.json";
const STATUS: &str = "native-status.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
struct Name(ContainerName);
impl TryFrom<String> for Name {
    type Error = owner::Error;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        Ok(Self(
            ContainerName::from_persisted(value).ok_or(owner::Error::Invalid)?,
        ))
    }
}
impl From<Name> for String {
    fn from(name: Name) -> Self {
        name.0.as_str().to_owned()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Gateway {
    generation: Generation,
    runtime: RuntimeKind,
    name: Name,
    recipe: owner::Recipe,
    data: NonZeroU16,
    management: NonZeroU16,
    key: Secret,
}
impl Gateway {
    pub(crate) fn client(&self) -> Client {
        Client::new(
            SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                self.management.get(),
            )),
            self.key.clone(),
        )
    }
    pub(crate) const fn generation(&self) -> Generation {
        self.generation
    }
    pub(crate) async fn ready(&self) -> Result<()> {
        let client = self.client();
        client
            .verify(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                self.data.get(),
            )))
            .await
            .map_err(io::Error::other)?;
        if client.status().await.map_err(io::Error::other)?.draining {
            return Err(owner::Error::Conflict.into());
        }
        if self.request(None).await? != 407 {
            return Err(owner::Error::Invalid.into());
        }
        let credential = Secret::generate().map_err(io::Error::other)?;
        let attachment = client
            .attach(
                credential.clone(),
                crate::gateway::policy::Policy::new(Vec::new()).map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?;
        if self.request(Some(&credential)).await? != 403 {
            return Err(owner::Error::Invalid.into());
        }
        drop(attachment);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if self.request(Some(&credential)).await? == 407 {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Operation { message: "native gateway ownership loss did not revoke the readiness grant; admission remains closed".into() });
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn request(&self, credential: Option<&Secret>) -> Result<u16> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut stream = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, self.data.get())).await?;
            let auth = credential.map_or_else(String::new, |credential| format!("Proxy-Authorization: Bearer {}\r\n", credential.expose()));
            stream.write_all(format!("GET http://wrix-readiness.invalid/ HTTP/1.1\r\nHost: wrix-readiness.invalid\r\n{auth}Connection: close\r\n\r\n").as_bytes()).await?;
            let mut bytes = Vec::new();
            stream.take(4097).read_to_end(&mut bytes).await?;
            if bytes.len() > 4096 { return Err(owner::Error::Invalid.into()); }
            let value = std::str::from_utf8(&bytes).map_err(|_| owner::Error::Invalid)?;
            value.lines().next().and_then(|line| line.split_whitespace().nth(1)).ok_or(owner::Error::Invalid)?.parse().map_err(|_| owner::Error::Invalid.into())
        }).await.map_err(io::Error::other)?
    }
    fn runtime(&self) -> Runtime {
        runtime(self.runtime)
    }
    pub(crate) fn verify(&self, deployment: &Id) -> Result<()> {
        let runtime = self.runtime();
        verify(&runtime, &self.name.0, &self.recipe)?;
        let ports = runtime.published_ports(self.name.0.as_str())?;
        if runtime
            .inspect_label(self.name.0.as_str(), "wrix.service.deployment")?
            .as_deref()
            != Some(deployment.to_string().as_str())
            || runtime
                .inspect_label(self.name.0.as_str(), "wrix.service.generation")?
                .as_deref()
                != Some(self.generation.number().to_string().as_str())
            || ![self.data.get(), self.management.get()]
                .iter()
                .all(|port| ports.contains(port))
        {
            return Err(owner::Error::Conflict.into());
        }
        Ok(())
    }
    fn retirement_file(&self) -> String {
        format!("native-retirement-{}.json", self.generation.number())
    }
    fn retirement(&self, authority: &Path) -> Result<Option<Retirement>> {
        let Some(bytes) = super::deployment::document(authority, &self.retirement_file())? else {
            return Ok(None);
        };
        let receipt: Retirement = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if receipt.version != 1
            || receipt.generation != self.generation
            || receipt.image != self.recipe.image_id
        {
            return Err(owner::Error::Conflict.into());
        }
        Ok(Some(receipt))
    }
    pub(crate) fn record_eligible(&self, authority: &Path) -> Result<()> {
        super::deployment::publish(
            authority,
            &self.retirement_file(),
            &Retirement {
                version: 1,
                generation: self.generation,
                image: self.recipe.image_id.clone(),
                phase: RetirementPhase::Eligible,
            },
        )
    }
    pub(crate) fn record_removed(&self, authority: &Path) -> Result<()> {
        if self.retirement(authority)?.is_none()
            || self.runtime().status(&self.name.0)? != RuntimeStatus::Missing
        {
            return Err(owner::Error::Conflict.into());
        }
        super::deployment::publish(
            authority,
            &self.retirement_file(),
            &Retirement {
                version: 1,
                generation: self.generation,
                image: self.recipe.image_id.clone(),
                phase: RetirementPhase::Removed,
            },
        )
    }
    pub(crate) fn eligible_stopped(&self, authority: &Path) -> Result<bool> {
        Ok(self.retirement(authority)?.is_some()
            && self.runtime().status(&self.name.0)? != RuntimeStatus::Running)
    }
    pub(crate) fn removed(&self, authority: &Path) -> Result<bool> {
        if self.retirement(authority)?.is_some()
            && self.runtime().status(&self.name.0)? == RuntimeStatus::Missing
        {
            self.record_removed(authority)?;
            return Ok(true);
        }
        Ok(false)
    }
    pub(crate) fn retire(&self, deployment: &Id) -> Result<()> {
        let runtime = self.runtime();
        let status = runtime.status(&self.name.0)?;
        if status == RuntimeStatus::Missing {
            return Ok(());
        }
        if status == RuntimeStatus::Running {
            self.verify(deployment)?;
            let output = Command::new(&runtime.binary)
                .args(["stop", self.name.0.as_str()])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: format!(
                        "gateway retirement stop failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                });
            }
        }
        if runtime.status(&self.name.0)? != RuntimeStatus::Stopped
            || runtime.container_image_id(&self.name.0)?.as_ref() != Some(&self.recipe.image_id)
        {
            return Err(owner::Error::Conflict.into());
        }
        let output = Command::new(&runtime.binary)
            .args(["rm", self.name.0.as_str()])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "gateway retirement removal failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetirementPhase {
    Eligible,
    Removed,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Retirement {
    version: u16,
    generation: Generation,
    image: wrix_sandbox::image::Digest,
    phase: RetirementPhase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Backend {
    generation: Generation,
    runtime: RuntimeKind,
    name: Name,
    recipe: owner::Recipe,
    sql: NonZeroU16,
    cache: NonZeroU16,
    compiler: NonZeroU16,
    sync: NonZeroU16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    version: u16,
    deployment: Id,
    gateway: Gateway,
    backend: Backend,
    backend_open: bool,
    obsolete: Vec<Gateway>,
    preparing: Option<Gateway>,
}
impl Plan {
    pub(crate) fn load(authority: &Path) -> Result<Option<Self>> {
        let Some(bytes) = super::deployment::document(authority, PLAN)? else {
            return Ok(None);
        };
        let plan: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if plan.version != 1
            || plan.obsolete.len() > 32
            || plan
                .obsolete
                .iter()
                .any(|old| old.generation == plan.gateway.generation)
            || plan.obsolete.iter().enumerate().any(|(index, old)| {
                plan.obsolete[..index]
                    .iter()
                    .any(|prior| prior.generation == old.generation)
            })
        {
            return Err(owner::Error::Invalid.into());
        }
        Ok(Some(plan))
    }
    fn digest(&self) -> Result<[u8; 32]> {
        Ok(Sha256::digest(serde_json::to_vec(self).map_err(io::Error::other)?).into())
    }
    fn publish(&self, authority: &Path) -> Result<()> {
        super::deployment::publish(authority, PLAN, self)
    }
    pub(crate) const fn deployment(&self) -> &Id {
        &self.deployment
    }
    pub(crate) const fn gateway(&self) -> &Gateway {
        &self.gateway
    }
    pub(crate) fn obsolete(&self) -> &[Gateway] {
        &self.obsolete
    }
    pub(crate) fn same_gateway(&self, other: &Self) -> bool {
        self.gateway == other.gateway
    }
    pub(crate) fn same_backend(&self, other: &Self) -> bool {
        self.backend == other.backend && self.backend_open == other.backend_open
    }
    pub(crate) const fn backend_open(&self) -> bool {
        self.backend_open
    }
    pub(crate) fn routes(&self) -> [SocketAddr; 5] {
        [
            self.backend.sql,
            self.backend.cache,
            self.gateway.data,
            self.backend.compiler,
            self.backend.sync,
        ]
        .map(|port| SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port.get())))
    }
    pub(crate) fn ready(&self, state: &Path) -> Result<()> {
        self.gateway.verify(&self.deployment)?;
        if !self.backend_open {
            return Ok(());
        }
        let runtime = runtime(self.backend.runtime);
        verify(&runtime, &self.backend.name.0, &self.backend.recipe)?;
        let ports = runtime.published_ports(self.backend.name.0.as_str())?;
        if ![
            self.backend.sql.get(),
            self.backend.cache.get(),
            self.backend.compiler.get(),
            self.backend.sync.get(),
        ]
        .iter()
        .all(|port| ports.contains(port))
        {
            return Err(owner::Error::Conflict.into());
        }
        let selected = owner::Selected {
            runtime: self.backend.runtime,
            workspace: None,
            container: self.backend.name.0.clone(),
            port: self.backend.cache,
            recipe: Some(self.backend.recipe.clone()),
        };
        owner::host::ready(
            state,
            &state.join("host-control/celler-issuer"),
            self.backend.cache,
        )?;
        owner::host::sql_ready(&runtime, &selected, state)?;
        let output = Command::new(&runtime.binary)
            .args([
                "exec",
                self.backend.name.0.as_str(),
                "wrix-shared-service",
                "compiler-ready",
            ])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "native compiler readiness failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(())
    }
    fn next(&self) -> Result<Generation> {
        let number = self
            .obsolete
            .iter()
            .map(|old| old.generation.number().get())
            .chain([
                self.gateway.generation.number().get(),
                self.backend.generation.number().get(),
            ])
            .max()
            .ok_or(owner::Error::Invalid)?;
        Ok(Generation::new(
            NonZeroU64::new(number.checked_add(1).ok_or(owner::Error::Invalid)?)
                .ok_or(owner::Error::Invalid)?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Activated,
    Draining,
    Failed,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Status {
    version: u16,
    epoch: [u8; 32],
    gateway: Generation,
    backend: Generation,
    backend_open: bool,
    pub(crate) state: State,
    pub(crate) retiring: usize,
    plan_digest: [u8; 32],
}
impl Status {
    pub(crate) fn publish(
        authority: &Path,
        plan: &Plan,
        state: State,
        retiring: usize,
    ) -> Result<()> {
        super::deployment::publish(
            authority,
            STATUS,
            &Self {
                version: 1,
                epoch: epoch(authority)?,
                gateway: plan.gateway.generation,
                backend: plan.backend.generation,
                backend_open: plan.backend_open,
                state,
                retiring,
                plan_digest: plan.digest()?,
            },
        )
    }
    fn matches(&self, plan: &Plan) -> Result<bool> {
        Ok(self.version == 1
            && self.plan_digest == plan.digest()?
            && self.gateway == plan.gateway.generation
            && self.backend == plan.backend.generation
            && self.backend_open == plan.backend_open
            && self.state != State::Failed)
    }
}

fn runtime(kind: RuntimeKind) -> Runtime {
    Runtime {
        binary: match kind {
            RuntimeKind::Podman => "podman",
            RuntimeKind::Container => "container",
        }
        .into(),
        kind,
        binding: None,
    }
}
fn verify(runtime: &Runtime, name: &ContainerName, recipe: &owner::Recipe) -> Result<()> {
    if runtime.status(name)? != RuntimeStatus::Running
        || runtime.container_image_id(name)?.as_ref() != Some(&recipe.image_id)
        || runtime
            .inspect_label(name.as_str(), "wrix.kind")?
            .as_deref()
            != Some("service")
        || runtime
            .inspect_label(name.as_str(), "wrix.service.scope")?
            .as_deref()
            != Some("host")
    {
        return Err(owner::Error::Conflict.into());
    }
    Ok(())
}
fn available() -> Result<NonZeroU16> {
    NonZeroU16::new(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port(),
    )
    .ok_or_else(|| owner::Error::Invalid.into())
}
fn authority(state: &Path) -> PathBuf {
    state.join("control/authority")
}
fn epoch(authority: &Path) -> Result<[u8; 32]> {
    super::deployment::document(authority, "native-epoch")?
        .ok_or(owner::Error::Invalid)?
        .try_into()
        .map_err(|_| owner::Error::Invalid.into())
}

pub(super) fn preflight_controller(host: &Host) -> Result<()> {
    let root = host.roots().state().join("control");
    let package = Package::from_env().map_err(io::Error::other)?;
    wrix_core::private_directory::prepare_with_parents(&root)?;
    let recorded = {
        let lifecycle = Lifecycle::lock(&root).map_err(io::Error::other)?;
        let native = Native::for_package(
            Platform::current().map_err(io::Error::other)?,
            fs::metadata(host.roots().state())?.uid(),
            &package,
        );
        lifecycle
            .preflight(&native, &package)
            .map_err(io::Error::other)?
    };
    if recorded.is_some() {
        controller(host)?;
    } else if crate::control::existing_deployment(host)
        .map_err(io::Error::other)?
        .is_some()
    {
        return Err(owner::Error::Invalid.into());
    }
    Ok(())
}

pub(super) fn controller(host: &Host) -> Result<[NonZeroU16; 5]> {
    let root = host.roots().state().join("control");
    let package = Package::from_env().map_err(io::Error::other)?;
    let selected = match Package::load(&root.join("package.json")) {
        Ok(package) => package,
        Err(crate::control::supervision::Error::Io(error))
            if error.kind() == io::ErrorKind::NotFound =>
        {
            package
        }
        Err(error) => return Err(io::Error::other(error).into()),
    };
    wrix_core::private_directory::prepare_with_parents(&root)?;
    let lifecycle = Lifecycle::lock(&root).map_err(io::Error::other)?;
    let native = Native::for_package(
        Platform::current().map_err(io::Error::other)?,
        fs::metadata(host.roots().state())?.uid(),
        &selected,
    );
    lifecycle
        .preflight(&native, &selected)
        .map_err(io::Error::other)?;
    lifecycle
        .start(&native, &selected)
        .map_err(io::Error::other)?;
    crate::control::maintenance::ready(host).map_err(io::Error::other)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(crate::control::Client::open(&root.join("authority")).map_err(io::Error::other)?.request(crate::control::Operation::InspectCore {})) {
        Ok(crate::control::Reply::CoreInterface { version: 2, ports, .. }) => Ok(ports),
        Ok(_) | Err(_) => Err(Error::Operation { message: "running controller does not provide compatible native four-role routing and handoff; run `wrix service stop --host`, then `wrix service update`; no backend was replaced".into() }),
    }
}

pub(crate) fn retained_sync_port(state: &Path) -> Result<Option<NonZeroU16>> {
    Ok(super::shared::Record::at(&state.join("host-lifecycle"))?
        .and_then(|record| record.public_sync_port()))
}

pub(super) fn guest_endpoint(state: &Path, role: crate::route::Role) -> Result<SocketAddrV4> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = crate::control::Client::open(&authority(state)).map_err(io::Error::other)?;
    match runtime.block_on(client.request(crate::control::Operation::InspectCore {})).map_err(io::Error::other)? {
        crate::control::Reply::CoreInterface { version: 2, ports, guest: Some(address) } if address.is_private() => {
            let index = match role {
                crate::route::Role::Sql => 0,
                crate::route::Role::CacheRead => 1,
                crate::route::Role::Egress => 2,
                crate::route::Role::Compiler => 3,
                crate::route::Role::Sync => 4,
            };
            Ok(SocketAddrV4::new(address, ports[index].get()))
        }
        _ => Err(Error::Operation { message: "the running controller has no verified stable Apple guest data route; explicit host maintenance is required, without direct-generation or wildcard fallback".into() }),
    }
}

fn start_gateway(gateway: &Gateway, deployment: &Id) -> Result<()> {
    let runtime = gateway.runtime();
    match runtime.status(&gateway.name.0)? {
        RuntimeStatus::Running => return gateway.verify(deployment),
        RuntimeStatus::Stopped => {
            let output = Command::new(&runtime.binary)
                .args(["start", gateway.name.0.as_str()])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
        }
        RuntimeStatus::Missing => {
            if !runtime.image_exists(gateway.recipe.binding.image())? {
                return Err(Error::Operation { message: "the recorded gateway image is unavailable; restore that exact bound image through explicit host recovery, never select a newer version implicitly".into() });
            }
            let mut command = Command::new(&runtime.binary);
            command.args(["run", "-d", "--name", gateway.name.0.as_str()]);
            if gateway.runtime == RuntimeKind::Podman {
                command.args(["--restart=unless-stopped", "--pull=never"]);
            }
            command
                .args([
                    "--label",
                    "wrix.kind=service",
                    "--label",
                    "wrix.service.scope=host",
                    "--label",
                    "wrix.service.role=gateway",
                    "--label",
                ])
                .arg(format!("wrix.service.deployment={deployment}"))
                .arg("--label")
                .arg(format!(
                    "wrix.service.generation={}",
                    gateway.generation.number()
                ))
                .arg("-p")
                .arg(format!("127.0.0.1:{}:3128", gateway.data))
                .arg("-p")
                .arg(format!("127.0.0.1:{}:3129", gateway.management))
                .args(["-e", "WRIX_GATEWAY_KEY"])
                .env("WRIX_GATEWAY_KEY", gateway.key.expose())
                .arg(gateway.recipe.image_id.as_str())
                .args(["wrix-shared-service", "gateway"]);
            let output = command.stdin(Stdio::null()).output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: format!(
                        "native gateway creation failed; exact generation retained: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                });
            }
        }
    }
    gateway.verify(deployment)
}

/// Native role entry. No host issuer files, worker policies or durable traffic grants are loaded.
pub async fn serve_gateway() -> Result<()> {
    let key =
        Secret::try_from(std::env::var("WRIX_GATEWAY_KEY").map_err(|_| owner::Error::Invalid)?)
            .map_err(io::Error::other)?;
    let data = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 3128)).await?;
    let management = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 3129)).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    crate::gateway::run(
        data,
        management,
        key,
        vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 3129))],
        NonZeroUsize::MIN.saturating_add(1023),
        async move {
            terminate.recv().await;
        },
    )
    .await
    .map_err(|error| io::Error::other(error).into())
}

pub(super) fn ensure(
    selected: &owner::Selected,
    sql: NonZeroU16,
    cache: NonZeroU16,
    compiler: NonZeroU16,
    sync: NonZeroU16,
) -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let public = controller(&host)?;
    if public[1] != selected.port {
        return Err(Error::Operation { message: "the retained stable cache endpoint was not bound by the controller; free that endpoint and repeat explicit host maintenance".into() });
    }
    let deployment = crate::control::existing_deployment(&host)
        .map_err(io::Error::other)?
        .ok_or(owner::Error::Invalid)?;
    let authority = authority(host.roots().state());
    let plan = if let Some(plan) = Plan::load(&authority)? {
        if plan.deployment != deployment || plan.backend.name.0 != selected.container {
            return Err(owner::Error::Conflict.into());
        }
        if plan.backend.recipe != *selected.recipe.as_ref().ok_or(owner::Error::Invalid)? {
            if !plan.backend_open
                && super::deployment::allows_update(
                    host.roots().state(),
                    &selected
                        .recipe
                        .as_ref()
                        .ok_or(owner::Error::Invalid)?
                        .binding,
                )?
            {
                return Ok(());
            }
            return Err(owner::Error::Conflict.into());
        }
        plan
    } else {
        let generation = Generation::new(NonZeroU64::MIN);
        let recipe = selected.recipe.clone().ok_or(owner::Error::Invalid)?;
        Plan {
            version: 1,
            deployment,
            gateway: Gateway {
                generation,
                runtime: selected.runtime,
                name: Name::try_from("wrix-gateway-1".to_owned())?,
                recipe: recipe.clone(),
                data: available()?,
                management: available()?,
                key: Secret::generate().map_err(io::Error::other)?,
            },
            backend: Backend {
                generation,
                runtime: selected.runtime,
                name: Name(selected.container.clone()),
                recipe,
                sql,
                cache,
                compiler,
                sync,
            },
            backend_open: true,
            obsolete: Vec::new(),
            preparing: None,
        }
    };
    plan.publish(&authority)?;
    start_gateway(&plan.gateway, &plan.deployment)?;
    wait(&authority, &plan)
}

fn wait(authority: &Path, plan: &Plan) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(bytes) = super::deployment::document(authority, STATUS)? {
            let status: Status = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if status.epoch == epoch(authority)? {
                if status.matches(plan)? {
                    return Ok(());
                }
                if status.version != 1 {
                    return Err(Error::Operation { message: "controller native attachment protocol is incompatible; exact role plan retained".into() });
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Operation { message: "native four-role routing readiness timed out; exact plan retained, no grants issued".into() });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn prepare_gateway(candidate: &super::binding::Candidate) -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let authority = authority(host.roots().state());
    let mut plan = Plan::load(&authority)?.ok_or(owner::Error::Invalid)?;
    if plan.gateway.recipe.binding == *candidate {
        if runtime(plan.backend.runtime).status(&plan.backend.name.0)? != RuntimeStatus::Running {
            plan.backend_open = false;
            plan.publish(&authority)?;
        }
        start_gateway(&plan.gateway, &plan.deployment)?;
        return wait(&authority, &plan);
    }
    if plan.obsolete.len() >= 32 {
        return Err(Error::Operation {
            message: "too many protected gateway generations remain; allow their streams to drain"
                .into(),
        });
    }
    let mut candidate_runtime = runtime(plan.gateway.runtime);
    candidate_runtime.binding = Some(candidate.clone());
    let prepared = candidate_runtime.ensure_image_at(host.roots().state())?;
    let recipe = prepared.clone_recipe();
    if recipe.config_digest == plan.gateway.recipe.config_digest {
        if runtime(plan.backend.runtime).status(&plan.backend.name.0)? != RuntimeStatus::Running {
            plan.backend_open = false;
            plan.publish(&authority)?;
        }
        start_gateway(&plan.gateway, &plan.deployment)?;
        return wait(&authority, &plan);
    }
    let generation = plan.next()?;
    let gateway = if let Some(gateway) = &plan.preparing {
        if gateway.recipe != recipe {
            return Err(owner::Error::Conflict.into());
        }
        gateway.clone()
    } else {
        let gateway = Gateway {
            generation,
            runtime: plan.gateway.runtime,
            name: Name::try_from(format!("wrix-gateway-{}", generation.number()))?,
            recipe,
            data: available()?,
            management: available()?,
            key: Secret::generate().map_err(io::Error::other)?,
        };
        plan.preparing = Some(gateway.clone());
        plan.publish(&authority)?;
        gateway
    };
    start_gateway(&gateway, &plan.deployment)?;
    if runtime(plan.backend.runtime).status(&plan.backend.name.0)? != RuntimeStatus::Running {
        plan.backend_open = false;
        if plan.gateway.runtime().status(&plan.gateway.name.0)? == RuntimeStatus::Stopped {
            plan.gateway.record_eligible(&authority)?;
        }
    }
    plan.obsolete.push(plan.gateway.clone());
    plan.gateway = gateway;
    plan.preparing = None;
    plan.publish(&authority)?;
    wait(&authority, &plan)
}

pub(super) fn activate_backend(
    selected: &owner::Selected,
    sql: NonZeroU16,
    cache: NonZeroU16,
    compiler: NonZeroU16,
    sync: NonZeroU16,
) -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let authority = authority(host.roots().state());
    let mut plan = Plan::load(&authority)?.ok_or(owner::Error::Invalid)?;
    plan.backend = Backend {
        generation: plan.gateway.generation,
        runtime: selected.runtime,
        name: Name(selected.container.clone()),
        recipe: selected.recipe.clone().ok_or(owner::Error::Invalid)?,
        sql,
        cache,
        compiler,
        sync,
    };
    plan.backend_open = true;
    plan.publish(&authority)?;
    wait(&authority, &plan)
}

pub(super) fn status(state: &Path) -> Result<String> {
    let authority = authority(state);
    let Some(plan) = Plan::load(&authority)? else {
        return Ok("native core generations: not provisioned\n".into());
    };
    let status = super::deployment::document(&authority, STATUS)?
        .map(|bytes| serde_json::from_slice::<Status>(&bytes))
        .transpose()
        .map_err(io::Error::other)?;
    Ok(format!(
        "gateway generation: {} ({})\nbackend generation: {} ({})\nbackend admission: {}\nnative activation: {:?}\nretiring gateways: {}\n",
        plan.gateway.generation.number(),
        plan.gateway.recipe.config_digest.as_str(),
        plan.backend.generation.number(),
        plan.backend.recipe.config_digest.as_str(),
        plan.backend_open,
        status.as_ref().map(|status| status.state),
        status
            .as_ref()
            .map_or(plan.obsolete.len(), |status| status.retiring)
    ))
}

pub(super) fn provisioned(state: &Path) -> Result<bool> {
    Ok(Plan::load(&authority(state))?.is_some())
}

pub(super) fn confirm_target(digest: &wrix_sandbox::image::Digest) -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let authority = authority(host.roots().state());
    let plan = Plan::load(&authority)?.ok_or(owner::Error::Invalid)?;
    if !plan.backend_open
        || plan.gateway.recipe.config_digest != *digest
        || plan.backend.recipe.config_digest != *digest
    {
        return Err(owner::Error::Conflict.into());
    }
    plan.ready(host.roots().state())?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(plan.gateway.ready())?;
    wait(&authority, &plan)
}

pub(super) fn reopen_backend() -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let authority = authority(host.roots().state());
    let mut plan = Plan::load(&authority)?.ok_or(owner::Error::Invalid)?;
    plan.backend_open = true;
    plan.publish(&authority)?;
    wait(&authority, &plan)
}

pub(super) fn close_backend() -> Result<()> {
    let host = Host::discover().map_err(io::Error::other)?;
    let authority = authority(host.roots().state());
    let mut plan = Plan::load(&authority)?.ok_or(owner::Error::Invalid)?;
    plan.backend_open = false;
    plan.publish(&authority)?;
    wait(&authority, &plan)
}

pub(super) fn owns(state: &Path, kind: RuntimeKind, name: &str) -> Result<bool> {
    let Some(plan) = Plan::load(&authority(state))? else {
        return Ok(false);
    };
    Ok(std::iter::once(&plan.gateway)
        .chain(&plan.obsolete)
        .chain(plan.preparing.as_ref())
        .any(|gateway| gateway.runtime == kind && gateway.name.0.as_str() == name))
}

pub(super) fn stop(host: &Host) -> Result<()> {
    let Some(plan) = Plan::load(&authority(host.roots().state()))? else {
        return Ok(());
    };
    for gateway in std::iter::once(&plan.gateway)
        .chain(&plan.obsolete)
        .chain(plan.preparing.as_ref())
    {
        let runtime = gateway.runtime();
        if runtime.status(&gateway.name.0)? == RuntimeStatus::Running {
            gateway.verify(&plan.deployment)?;
            let output = Command::new(&runtime.binary)
                .args(["stop", gateway.name.0.as_str()])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: "native gateway host shutdown failed; selection and data retained"
                        .into(),
                });
            }
        }
        if gateway.generation != plan.gateway.generation
            && plan
                .obsolete
                .iter()
                .any(|old| old.generation == gateway.generation)
            && runtime.status(&gateway.name.0)? == RuntimeStatus::Stopped
        {
            gateway.record_eligible(&authority(host.roots().state()))?;
        }
    }
    File::open(authority(host.roots().state()))?.sync_all()?;
    Ok(())
}
