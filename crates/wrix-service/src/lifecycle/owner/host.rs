//! Host-user Celler creation and explicit recovery, independent of repository admission.

use std::{
    fs,
    num::NonZeroU16,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use super::super::{
    CACHE_BACKEND_LABEL, CACHE_BACKEND_VERSION, CACHE_ENABLED_LABEL, CACHE_PORT_START,
    CACHE_PORT_WIDTH, Error, Result, Runtime, RuntimeKind, RuntimeStatus, binding, coordination,
    is_loopback_port_available,
};
use super::Selected;
use wrix_cache::backend::{
    issuer::Authority, layout::Layout, publication::registration::provision_common,
};
use wrix_core::path::ContainerName;

pub(in crate::lifecycle) fn recover() -> Result<String> {
    let roots = wrix_core::host_path::Roots::discover()?;
    let guard = coordination::acquire(&roots.state().join("host-lifecycle"))?;
    let previous = Selected::read(guard.root())?;
    let pending = Selected::read(&guard.root().join("celler-candidate"))?;
    let recorded = previous
        .as_ref()
        .or(pending.as_ref())
        .ok_or(Error::RecoveryScope)?;
    let mut runtime = Runtime::from_env()?;
    if runtime.kind != recorded.runtime {
        runtime.kind = recorded.runtime;
        runtime.binary = match recorded.runtime {
            RuntimeKind::Podman => "podman",
            RuntimeKind::Container => "container",
        }
        .into();
    }
    let selected = ensure(&runtime, &guard, previous.as_ref(), None)?;
    Ok(format!(
        "Celler owner: {}\nruntime: Running\n",
        selected.container
    ))
}

/// The caller retains host lifecycle exclusion through readiness and selection commit.
pub(in crate::lifecycle) fn ensure(
    runtime: &Runtime,
    guard: &coordination::Guard,
    previous: Option<&Selected>,
    bootstrap_port: Option<NonZeroU16>,
) -> Result<Selected> {
    let root = guard
        .root()
        .parent()
        .ok_or(binding::Error::Metadata("missing host state root"))?;
    let candidate_root = guard.root().join("celler-candidate");
    let candidate = Selected::read(&candidate_root)?;
    if let (Some(candidate), Some(previous)) = (&candidate, previous)
        && (candidate.runtime != previous.runtime
            || candidate.container != previous.container
            || candidate.port != previous.port)
    {
        return Err(super::Error::Conflict.into());
    }
    let selected = match candidate.as_ref().or(previous) {
        Some(owner) => owner.clone(),
        None => Selected {
            runtime: runtime.kind,
            workspace: None,
            container: ContainerName::from_persisted("wrix-host-service".into())
                .ok_or(super::Error::Invalid)?,
            port: bootstrap_port
                .or_else(|| {
                    (CACHE_PORT_START..CACHE_PORT_START + CACHE_PORT_WIDTH)
                        .find(|port| is_loopback_port_available(*port))
                        .and_then(NonZeroU16::new)
                })
                .ok_or(Error::NoAvailableLoopbackPort {
                    start: CACHE_PORT_START,
                    end: CACHE_PORT_START + CACHE_PORT_WIDTH - 1,
                })?,
            recipe: None,
        },
    };
    if selected.runtime != runtime.kind || Selected::read(guard.root())?.as_ref() != previous {
        return Err(super::Error::Conflict.into());
    }
    let alternate = runtime.alternate();
    for (backend, local) in [(&alternate, false), (runtime, true)] {
        if let Some(containers) = backend.available_container_infos()? {
            for container in containers {
                if container.kind.as_deref() != Some("service") {
                    continue;
                }
                if local && container.name == selected.container.as_str() {
                    continue;
                }
                if backend.status(
                    &ContainerName::from_persisted(container.name.clone())
                        .ok_or(super::Error::Invalid)?,
                )? == RuntimeStatus::Running
                    && backend
                        .inspect_label(&container.name, CACHE_BACKEND_LABEL)?
                        .is_some()
                    && backend
                        .inspect_label(&container.name, CACHE_ENABLED_LABEL)?
                        .as_deref()
                        != Some("false")
                {
                    return Err(Error::CellerOwnership {
                        container: container.name,
                        reason: super::super::OwnershipFailure::Competing,
                    });
                }
            }
        }
    }
    let status = runtime.status(&selected.container)?;
    if status != RuntimeStatus::Missing {
        if previous.is_none() && candidate.is_none() {
            return Err(Error::CellerOwnership {
                container: selected.container.as_str().into(),
                reason: super::super::OwnershipFailure::Identity,
            });
        }
        validate(runtime, &selected, status)?;
    }
    let layout = Layout::current()?;
    let issuer = root.join("host-control/celler-issuer");
    if previous.is_some() || candidate.is_some() {
        Authority::open(&issuer).map_err(super::super::cache::Error::from)?;
    }
    layout.prepare()?;
    Authority::provision(&issuer, layout.metadata()).map_err(super::super::cache::Error::from)?;
    if status == RuntimeStatus::Running {
        ready(root, &issuer, selected.port)?;
        if candidate.is_some() {
            commit(&selected, previous, guard)?;
            fs::remove_file(candidate_root.join("celler-owner.json"))?;
        }
        return Ok(selected);
    }
    runtime.binding.as_ref().ok_or(binding::Error::Missing)?;
    if status == RuntimeStatus::Stopped {
        runtime.remove(&selected.container)?;
    }
    wrix_cache::backend::layout::runtime::install(layout.metadata())?;
    let image = runtime.ensure_image_at(root)?;
    let recovered = Selected {
        workspace: None,
        recipe: image.clone_recipe(),
        ..selected.clone()
    };
    wrix_core::private_directory::prepare_with_parents(&candidate_root)?;
    recovered.persist(&candidate_root)?;
    let mut command = Command::new(&runtime.binary);
    command.args(["run", "-d", "--name", selected.container.as_str()]);
    if runtime.kind == RuntimeKind::Podman {
        command.args(["--restart=unless-stopped", "--pull=never"]);
    }
    command
        .args([
            "--label",
            "wrix.kind=service",
            "--label",
            "wrix.service.scope=host",
            "--label",
            "wrix.cache.enabled=true",
        ])
        .arg("--label")
        .arg(format!("{CACHE_BACKEND_LABEL}={CACHE_BACKEND_VERSION}"))
        .arg("-p")
        .arg(format!("127.0.0.1:{}:8080", selected.port));
    for mount in layout.mounts() {
        command.arg("-v").arg(format!(
            "{}:{}:rw",
            mount.source.display(),
            mount.destination
        ));
    }
    if let Some(workspace) = &selected.workspace {
        command
            .arg("--label")
            .arg(format!("wrix.workspace.hash={workspace}"));
    }
    let output = command
        .arg(image.as_str())
        .args(["sh", "-c", wrix_cache::backend::layout::runtime::COMMAND])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::Operation {
            message: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    ready(root, &issuer, selected.port)?;
    commit(&recovered, previous, guard)?;
    fs::remove_file(candidate_root.join("celler-owner.json"))?;
    Ok(recovered)
}

fn commit(
    selected: &Selected,
    previous: Option<&Selected>,
    guard: &coordination::Guard,
) -> Result<()> {
    if Selected::read(guard.root())?.as_ref() != previous {
        return Err(super::Error::Conflict.into());
    }
    selected.persist(guard.root())?;
    Ok(())
}

fn validate(runtime: &Runtime, owner: &Selected, status: RuntimeStatus) -> Result<()> {
    let identity = match &owner.workspace {
        Some(workspace) => {
            runtime
                .inspect_label(owner.container.as_str(), "wrix.workspace.hash")?
                .as_deref()
                == Some(workspace.as_str())
        }
        None => {
            runtime
                .inspect_label(owner.container.as_str(), "wrix.service.scope")?
                .as_deref()
                == Some("host")
        }
    };
    if !identity
        || runtime
            .inspect_label(owner.container.as_str(), "wrix.kind")?
            .as_deref()
            != Some("service")
        || runtime
            .inspect_label(owner.container.as_str(), CACHE_ENABLED_LABEL)?
            .as_deref()
            != Some("true")
        || runtime
            .inspect_label(owner.container.as_str(), CACHE_BACKEND_LABEL)?
            .as_deref()
            != Some(CACHE_BACKEND_VERSION)
        || (status == RuntimeStatus::Running
            && !runtime
                .published_ports(owner.container.as_str())?
                .contains(&owner.port.get()))
        || (owner.recipe.is_some()
            && runtime.container_image_id(&owner.container)?
                != owner.recipe.as_ref().map(|recipe| recipe.image_id.clone()))
    {
        return Err(Error::CellerOwnership {
            container: owner.container.as_str().into(),
            reason: super::super::OwnershipFailure::Identity,
        });
    }
    Ok(())
}

fn ready(root: &Path, issuer: &Path, port: NonZeroU16) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let endpoint = format!("http://127.0.0.1:{port}/");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match provision_common(
                    &root.join("workspaces/common"),
                    &endpoint,
                    issuer,
                    Duration::from_hours(720),
                    Duration::from_secs(1),
                )
                .await
                {
                    Ok(_) => return Ok(()),
                    Err(wrix_cache::backend::publication::Error::Backend(
                        wrix_cache::backend::Error::Transport,
                    )) => tokio::time::sleep(Duration::from_millis(200)).await,
                    Err(error) => return Err(super::super::cache::Error::Registration(error)),
                }
            }
        })
        .await
        .map_err(|_| super::super::cache::Error::Deadline)?
    })?;
    Ok(())
}
