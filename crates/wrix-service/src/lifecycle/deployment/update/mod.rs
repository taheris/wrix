//! Exact package intent survives failed activation; readiness is native evidence, never an image inventory assertion.

use super::{document, publish};
use crate::control::supervision::{Lifecycle, Native, Package, Platform};
use crate::lifecycle::{Error, Result, Runtime, RuntimeStatus, binding, coordination, owner};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io,
    os::unix::fs::MetadataExt,
    process::{Command, Stdio},
};
use wrix_state::host::Host;

mod cancel;
pub use cancel::run as cancel;

const FILE: &str = "update.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(super) struct Attempt(String);
impl Attempt {
    fn new() -> Result<Self> {
        use std::io::Read;
        let mut bytes = [0_u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let mut encoded = String::with_capacity(32);
        for byte in bytes {
            use std::fmt::Write;
            write!(&mut encoded, "{byte:02x}").map_err(io::Error::other)?;
        }
        Ok(Self(encoded))
    }
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Attempt {
    type Error = owner::Error;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(owner::Error::Invalid);
        }
        Ok(Self(value))
    }
}
impl From<Attempt> for String {
    fn from(attempt: Attempt) -> Self {
        attempt.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    locks: Vec<super::ownership::Record>,
    origin: Option<wrix_sandbox::image::Digest>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Preparing,
    Draining,
    Releasing,
    Released,
    Starting,
    Checking,
    Activated,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u16,
    deployment: crate::deployment::Id,
    attempt: Attempt,
    candidate: binding::Candidate,
    controller: Package,
    pools: Pools,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ownership: Option<Ownership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<Failure>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Failure {
    BackendDrain,
    NativeReadiness,
    PoolReadiness,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Pools {
    Unprovisioned,
    Bound {
        candidate: crate::nix_store::image::Candidate,
        tools: crate::nix_store::owner::backend::tools::Tools,
    },
}

impl Pools {
    fn from_env(host: &Host) -> Result<Self> {
        match (
            std::env::var_os("WRIX_NIX_POOL_PACKAGE_BINDING"),
            std::env::var_os("WRIX_NIX_POOL_HOST_TOOLS"),
        ) {
            (None, None) if !crate::nix_store::lifecycle::needs_binding(host).map_err(io::Error::other)? => Ok(Self::Unprovisioned),
            (Some(_), Some(_)) => Ok(Self::Bound {
                candidate: crate::nix_store::image::Candidate::from_env().map_err(io::Error::other)?,
                tools: crate::nix_store::owner::backend::tools::Tools::from_env().map_err(io::Error::other)?,
            }),
            _ => Err(Error::Operation { message: "provisioned pool update requires the invoked host package's complete pool-image and native-tools binding; no pool or core owner was changed".into() }),
        }
    }

    fn begin<'a>(
        &self,
        host: &'a Host,
        controller: &Package,
    ) -> Result<crate::nix_store::lifecycle::Update<'a>> {
        match self {
            Self::Unprovisioned => crate::nix_store::lifecycle::Update::unprovisioned(host),
            Self::Bound { candidate, tools } => {
                crate::nix_store::lifecycle::Update::begin(host, controller, candidate, tools)
            }
        }
        .map_err(|error| io::Error::other(error).into())
    }
}

impl Intent {
    fn check_version(&self) -> Result<()> {
        if !matches!(self.version, 1 | 2) {
            return Err(owner::Error::Invalid.into());
        }
        Ok(())
    }

    fn check_cancellable(&self) -> Result<()> {
        self.check_version()?;
        let prepared = self.phase == Phase::Prepared && self.failure.is_none();
        if self.ownership.is_some() || !(prepared || self.legacy_refusal()) {
            return Err(Error::Operation { message: "update has progressed or has uncertain effects; cancellation refused. Retry `wrix service update` with its recorded exact host package; retained intent and ownership are unchanged".into() });
        }
        Ok(())
    }

    fn legacy_refusal(&self) -> bool {
        matches!(self.phase, Phase::Prepared | Phase::Preparing)
            && self.failure == Some(Failure::BackendDrain)
    }

    fn same_target(&self, other: &Self) -> bool {
        self.version == other.version
            && self.deployment == other.deployment
            && self.candidate == other.candidate
            && self.controller == other.controller
            && self.pools == other.pools
    }
}

pub fn run() -> Result<String> {
    let host = Host::discover().map_err(io::Error::other)?;
    let deployment = crate::control::existing_deployment(&host).map_err(io::Error::other)?
        .ok_or_else(|| Error::Operation { message: "service update requires an existing deployment; run `wrix service start` from a repository first".into() })?;
    let _update = coordination::acquire(&host.roots().state().join("service-update"))?;
    let root = host.roots().state().join("host-lifecycle");
    let mut intent = Intent {
        version: 2,
        deployment,
        attempt: Attempt::new()?,
        candidate: binding::Candidate::from_env()?,
        controller: Package::from_env().map_err(io::Error::other)?,
        pools: Pools::from_env(&host)?,
        phase: Phase::Prepared,
        ownership: None,
        failure: None,
    };
    {
        let guard = coordination::acquire(&root)?;
        if let Some(bytes) = document(guard.root(), FILE)? {
            let retained: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            retained.check_version()?;
            intent.version = retained.version;
            if !retained.same_target(&intent) {
                return Err(Error::Operation { message: "an interrupted whole-deployment update retains a different exact package/image intent; inspect `wrix service --host` and use `wrix service update --cancel` only for a provably unstarted update. Otherwise repeat its exact recorded package before selecting another target".into() });
            }
            intent = retained;
        } else {
            owner::Selected::read(guard.root())?.ok_or(owner::Error::Invalid)?;
            publish(guard.root(), FILE, &intent)?;
        }
    }
    let digest = intent.candidate.realize(&root.join("update-roots"))?;
    begin_preparing(&root, &mut intent)?;
    let control = host.roots().state().join("control");
    let native = Native::for_package(
        Platform::current().map_err(io::Error::other)?,
        fs::metadata(host.roots().state())?.uid(),
        &intent.controller,
    );
    native
        .prepare_upgrade(&control, &intent.controller)
        .map_err(io::Error::other)?;
    let pools = intent.pools.begin(&host, &intent.controller)?;
    let guard = coordination::acquire(&root)?;
    let bytes = document(guard.root(), FILE)?.ok_or(owner::Error::Conflict)?;
    let current: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if current != intent {
        return Err(owner::Error::Conflict.into());
    }
    let previous = owner::Selected::read(guard.root())?.ok_or(owner::Error::Invalid)?;
    let selected =
        if let Some(candidate) = owner::Selected::read(&guard.root().join("celler-candidate"))? {
            if candidate.runtime != previous.runtime
                || candidate.container != previous.container
                || candidate.port != previous.port
                || candidate.recipe.as_ref().is_none_or(|recipe| {
                    recipe.binding != intent.candidate || recipe.config_digest != digest
                })
            {
                return Err(owner::Error::Conflict.into());
            }
            candidate
        } else {
            previous.clone()
        };
    let mut runtime = Runtime::from_env()?;
    if runtime.kind != selected.runtime {
        runtime = runtime.alternate();
    }
    runtime.binding = Some(intent.candidate.clone());
    super::validate_inventory(&runtime, &selected)?;
    let mut status = runtime.status(&selected.container)?;
    let same_image = selected
        .recipe
        .as_ref()
        .is_some_and(|recipe| recipe.config_digest == digest);
    let native_core = super::super::generation::provisioned(host.roots().state())?;
    if status == RuntimeStatus::Running && !native_core {
        intent.failure = Some(Failure::BackendDrain);
        publish(guard.root(), FILE, &intent)?;
        return Err(Error::Operation { message: "the running legacy backend predates native whole-deployment ownership/drain interfaces; upgrading requires explicit host-wide maintenance: `wrix service stop --host`, then repeat `wrix service update` with this exact package. To abandon this attempt without downtime, use current Wrix `service update --cancel` if its original-owner evidence checks pass. No core owner was stopped or replaced".into() });
    }
    intent.failure = None;
    publish(guard.root(), FILE, &intent)?;
    let stopped = control.join("stopped").try_exists()?;
    if stopped {
        let controller = Lifecycle::lock(&control).map_err(io::Error::other)?;
        controller
            .upgrade(&native, &intent.controller, || {
                crate::control::recover_registry(&host).map_err(io::Error::other)
            })
            .map_err(io::Error::other)?;
    } else {
        crate::control::maintenance::ready(&host).map_err(io::Error::other)?;
    }
    if native_core {
        super::super::generation::prepare_gateway(&intent.candidate)?;
    }
    let replace = !same_image || !native_core;
    if replace {
        let layout = wrix_cache::backend::layout::Layout::current()?;
        let pins = super::ownership::Pin::capture(host.roots().state(), layout.metadata())?;
        let locks = pins
            .iter()
            .map(super::ownership::Pin::record)
            .collect::<Vec<_>>();
        if let Some(shutdown) = super::Shutdown::read(guard.root())?
            && shutdown.locks != locks
        {
            return Err(Error::Operation { message: "stopped deployment storage ownership changed before update; no candidate opened its data".into() });
        }
        if let Some(ownership) = &intent.ownership {
            if ownership.locks != locks {
                return Err(owner::Error::Conflict.into());
            }
        } else {
            intent.ownership = Some(Ownership {
                locks,
                origin: selected
                    .recipe
                    .as_ref()
                    .map(|recipe| recipe.image_id.clone())
                    .or(runtime.container_image_id(&selected.container)?),
            });
            publish(guard.root(), FILE, &intent)?;
        }
        if status == RuntimeStatus::Running || intent.phase == Phase::Releasing {
            if !matches!(
                intent.phase,
                Phase::Releasing | Phase::Released | Phase::Starting
            ) {
                intent.phase = Phase::Draining;
                publish(guard.root(), FILE, &intent)?;
                super::super::generation::close_backend()?;
                if let Err(error) = super::backend::quiesce(&runtime, &selected) {
                    intent.failure = Some(Failure::BackendDrain);
                    publish(guard.root(), FILE, &intent)?;
                    if super::backend::abort(&runtime, &selected).is_ok() {
                        super::super::generation::reopen_backend()?;
                    }
                    return Err(error);
                }
                intent.phase = Phase::Releasing;
                publish(guard.root(), FILE, &intent)?;
            }
            super::backend::release(&runtime, &selected, host.roots().state())?;
            status = RuntimeStatus::Stopped;
        }
        for pin in &pins {
            pin.confirm()?;
        }
        let ownership = intent.ownership.as_ref().ok_or(owner::Error::Invalid)?;
        super::checkpoint::create(
            host.roots().state(),
            &intent.attempt,
            &intent.candidate,
            &digest,
            ownership.origin.as_ref(),
        )?;
        intent.phase = Phase::Released;
        publish(guard.root(), FILE, &intent)?;
        if status == RuntimeStatus::Stopped {
            let output = Command::new(&runtime.binary)
                .args(["rm", selected.container.as_str()])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation { message: "native released-owner removal failed; data, original ownership and checkpoint retained".into() });
            }
        }
        drop(pins);
        intent.phase = Phase::Starting;
        publish(guard.root(), FILE, &intent)?;
    }
    let active = owner::host::ensure(&runtime, &guard, Some(&previous), None, true)?;
    if active
        .recipe
        .as_ref()
        .is_none_or(|recipe| recipe.config_digest != digest)
    {
        return Err(owner::Error::Conflict.into());
    }
    intent.phase = Phase::Checking;
    publish(guard.root(), FILE, &intent)?;
    let (sql, cache, compiler, sync) = super::super::shared::Record::at(guard.root())?
        .and_then(|record| record.native_ports())
        .ok_or(owner::Error::Invalid)?;
    if let Err(error) =
        super::super::generation::activate_backend(&active, sql, cache, compiler, sync)
            .and_then(|()| crate::lifecycle::generation::confirm_target(&digest))
    {
        intent.failure = Some(Failure::NativeReadiness);
        publish(guard.root(), FILE, &intent)?;
        return Err(error);
    }
    intent.phase = Phase::Activated;
    publish(guard.root(), FILE, &intent)?;
    if let Err(error) = pools.complete() {
        intent.failure = Some(Failure::PoolReadiness);
        publish(guard.root(), FILE, &intent)?;
        return Err(io::Error::other(error).into());
    }
    if super::Shutdown::read(guard.root())?.is_some() {
        fs::remove_file(guard.root().join(super::SHUTDOWN))?;
    }
    fs::remove_file(guard.root().join(FILE))?;
    File::open(guard.root())?.sync_all()?;
    Ok(format!(
        "scope: host-user\ndeployment: {}\nselected image: {} ({})\ncore roles: activated (gateway, Dolt, Celler, compiler)\nprovisioned pools: ready\nobsolete gateway streams: may still be draining; cleanup continues automatically under controller supervision\nCeller/compiler clients may reconnect at unchanged endpoints after admitted work drains\n",
        intent.deployment,
        intent.candidate.image().as_str(),
        digest.as_str()
    ))
}

fn begin_preparing(root: &std::path::Path, intent: &mut Intent) -> Result<()> {
    if intent.phase == Phase::Prepared {
        let guard = coordination::acquire(root)?;
        let bytes = document(guard.root(), FILE)?.ok_or(owner::Error::Conflict)?;
        let current: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if current != *intent {
            return Err(owner::Error::Conflict.into());
        }
        intent.phase = Phase::Preparing;
        publish(guard.root(), FILE, intent)?;
    }
    Ok(())
}

pub(super) fn allows(state: &std::path::Path, candidate: &binding::Candidate) -> Result<bool> {
    let Some(bytes) = document(&state.join("host-lifecycle"), FILE)? else {
        return Ok(false);
    };
    let intent: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    intent.check_version()?;
    Ok(intent.candidate == *candidate)
}

pub(super) fn status(root: &std::path::Path) -> Result<String> {
    let Some(bytes) = document(root, FILE)? else {
        return Ok("update: none recorded\n".into());
    };
    let intent: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    intent.check_version()?;
    Ok(format!(
        "update: {:?}\nupdate failure: {:?}\nupdate target: {}\n",
        intent.phase,
        intent.failure,
        intent.candidate.image().as_str()
    ))
}

#[cfg(test)]
mod test {
    use super::*;

    fn intent() -> Intent {
        let candidate = serde_json::json!({
            "schema": 1,
            "image": "localhost/wrix-service:bound",
            "source_kind": "nix-descriptor",
            "source": {"path": "/source", "derivation": "/source.drv"},
            "digest": {"path": "/digest", "derivation": "/digest.drv"}
        });
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "deployment": "b".repeat(32),
            "attempt": "c".repeat(32),
            "candidate": candidate,
            "pools": {"state": "bound", "candidate": candidate, "tools": {"version": 1, "root": "/nix/store/00000000000000000000000000000000-runtime", "system": "x86_64-linux"}},
            "controller": {
                "controller": "/nix/store/00000000000000000000000000000000-controller",
                "runtime": "/nix/store/00000000000000000000000000000000-runtime",
                "image": "/nix/store/00000000000000000000000000000000-image.drv"
            },
            "phase": "checking",
            "failure": "native_readiness"
        })).unwrap()
    }

    fn prepared() -> Intent {
        let mut pending = intent();
        pending.version = 2;
        pending.phase = Phase::Prepared;
        pending.failure = None;
        pending
    }

    #[test]
    fn cancel_retains_exact_intent_before_clearing_the_admission_fence() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let pending = prepared();
        publish(root.path(), FILE, &pending).unwrap();
        let bytes = document(root.path(), FILE).unwrap().unwrap();
        let retained = cancel::archive(root.path(), &bytes, &pending).unwrap();
        assert_eq!(fs::read(&retained).unwrap(), bytes);
        assert_eq!(fs::metadata(&retained).unwrap().mode() & 0o777, 0o600);
        assert!(document(root.path(), FILE).unwrap().is_none());
        assert_eq!(status(root.path()).unwrap(), "update: none recorded\n");
    }

    #[test]
    fn cancel_retry_accepts_durable_evidence_from_before_fence_removal() {
        let root = tempfile::tempdir().unwrap();
        let pending = prepared();
        publish(root.path(), FILE, &pending).unwrap();
        let bytes = document(root.path(), FILE).unwrap().unwrap();
        let archive = format!("cancelled-update-{}.json", pending.attempt.as_str());
        publish(root.path(), &archive, &pending).unwrap();
        cancel::archive(root.path(), &bytes, &pending).unwrap();
        assert_eq!(document(root.path(), &archive).unwrap().unwrap(), bytes);
    }

    #[test]
    fn conflicting_cancellation_evidence_preserves_both_documents() {
        let root = tempfile::tempdir().unwrap();
        let pending = prepared();
        publish(root.path(), FILE, &pending).unwrap();
        let bytes = document(root.path(), FILE).unwrap().unwrap();
        let archive = format!("cancelled-update-{}.json", pending.attempt.as_str());
        publish(
            root.path(),
            &archive,
            &serde_json::json!({"unrelated": true}),
        )
        .unwrap();
        let evidence = document(root.path(), &archive).unwrap().unwrap();
        assert!(cancel::archive(root.path(), &bytes, &pending).is_err());
        assert_eq!(document(root.path(), FILE).unwrap().unwrap(), bytes);
        assert_eq!(document(root.path(), &archive).unwrap().unwrap(), evidence);
    }

    #[test]
    fn cancellation_refuses_every_progressed_phase_and_retains_intent() {
        for phase in [
            Phase::Preparing,
            Phase::Draining,
            Phase::Releasing,
            Phase::Released,
            Phase::Starting,
            Phase::Checking,
            Phase::Activated,
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut pending = prepared();
            pending.phase = phase;
            publish(root.path(), FILE, &pending).unwrap();
            let bytes = document(root.path(), FILE).unwrap().unwrap();
            assert!(cancel::archive(root.path(), &bytes, &pending).is_err());
            assert_eq!(document(root.path(), FILE).unwrap().unwrap(), bytes);
        }
    }

    #[test]
    fn cancellation_refuses_unknown_version_readiness_failure_or_ownership_even_when_prepared() {
        let mut unknown = prepared();
        unknown.version = 3;
        let mut failed = prepared();
        failed.failure = Some(Failure::NativeReadiness);
        let mut pool_failed = prepared();
        pool_failed.failure = Some(Failure::PoolReadiness);
        let mut released = prepared();
        released.ownership = Some(Ownership {
            locks: Vec::new(),
            origin: None,
        });
        for pending in [unknown, failed, pool_failed, released] {
            assert!(pending.check_cancellable().is_err());
        }
    }

    #[test]
    fn native_preparation_is_durably_non_cancellable_before_starting() {
        let root = tempfile::tempdir().unwrap();
        let mut pending = prepared();
        publish(root.path(), FILE, &pending).unwrap();
        begin_preparing(root.path(), &mut pending).unwrap();
        let retained: Intent =
            serde_json::from_slice(&document(root.path(), FILE).unwrap().unwrap()).unwrap();
        assert_eq!(retained.phase, Phase::Preparing);
        assert!(retained.check_cancellable().is_err());
        assert!(retained.same_target(&pending));
    }

    #[test]
    fn native_preparation_cannot_resurrect_cancelled_intent() {
        let root = tempfile::tempdir().unwrap();
        let mut pending = prepared();
        publish(root.path(), FILE, &pending).unwrap();
        let bytes = document(root.path(), FILE).unwrap().unwrap();
        cancel::archive(root.path(), &bytes, &pending).unwrap();
        assert!(begin_preparing(root.path(), &mut pending).is_err());
        assert!(document(root.path(), FILE).unwrap().is_none());
    }

    #[test]
    fn failed_activation_persists_the_exact_target_and_phase_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let failed = intent();
        publish(root.path(), FILE, &failed).unwrap();
        let bytes = document(root.path(), FILE).unwrap().unwrap();
        let retained: Intent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(retained, failed);
        let mut retry = failed;
        retry.failure = None;
        retry.phase = Phase::Prepared;
        assert!(retained.same_target(&retry));
        let report = status(root.path()).unwrap();
        assert!(report.contains("Checking"));
        assert!(report.contains("NativeReadiness"));
        assert_eq!(fs::read(root.path().join(FILE)).unwrap(), bytes);
    }

    #[test]
    fn activation_retry_cannot_change_any_bound_software_target() {
        let original = intent();
        for pool in [false, true] {
            let mut changed = serde_json::to_value(&original).unwrap();
            let candidate = if pool {
                &mut changed["pools"]["candidate"]
            } else {
                &mut changed["candidate"]
            };
            candidate["image"] = serde_json::json!("localhost/other:bound");
            assert!(!original.same_target(&serde_json::from_value(changed).unwrap()));
        }
        for field in ["controller", "runtime", "image"] {
            let mut changed = serde_json::to_value(&original).unwrap();
            changed["controller"][field] = serde_json::json!(if field == "image" {
                "/nix/store/11111111111111111111111111111111-other.drv"
            } else {
                "/nix/store/11111111111111111111111111111111-other"
            });
            assert!(!original.same_target(&serde_json::from_value(changed).unwrap()));
        }
        let mut changed = serde_json::to_value(&original).unwrap();
        changed["pools"]["tools"]["root"] =
            serde_json::json!("/nix/store/11111111111111111111111111111111-other");
        assert!(!original.same_target(&serde_json::from_value(changed).unwrap()));
    }
}
