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
    let host = wrix_state::host::Host::discover().map_err(std::io::Error::other)?;
    let _pools = crate::nix_store::lifecycle::gate::shared(&host).map_err(std::io::Error::other)?;
    let guard = coordination::acquire(&host.roots().state().join("host-lifecycle"))?;
    let previous = Selected::read(guard.root())?;
    let pending = Selected::read(&guard.root().join("celler-candidate"))?;
    let recorded = previous.as_ref().or(pending.as_ref());
    let mut runtime = Runtime::from_env()?;
    if let Some(recorded) = recorded
        && runtime.kind != recorded.runtime
    {
        runtime.kind = recorded.runtime;
        runtime.binary = match recorded.runtime {
            RuntimeKind::Podman => "podman",
            RuntimeKind::Container => "container",
        }
        .into();
    }
    // Policy compatibility must be admitted before creating or starting any backend.
    crate::control::recover_registry(&host).map_err(std::io::Error::other)?;
    let selected = ensure(&runtime, &guard, previous.as_ref(), None, true)?;
    Ok(format!(
        "Celler owner: {}\nruntime: Running\n",
        selected.container
    ))
}

pub(in crate::lifecycle) fn repair() -> Result<String> {
    let host = wrix_state::host::Host::discover().map_err(std::io::Error::other)?;
    Runtime::from_env()?;
    // Controller maintenance has its own lifecycle/kernel/registry exclusion.
    // Do not retain the backend lock across closure builds or controller startup.
    crate::control::maintenance::repair(&host).map_err(std::io::Error::other)?;
    // Recovery revalidates inventory and selection under its own host transaction.
    recover()
}

/// The caller retains host lifecycle exclusion through readiness and selection commit.
pub(in crate::lifecycle) fn ensure(
    runtime: &Runtime,
    guard: &coordination::Guard,
    previous: Option<&Selected>,
    bootstrap_port: Option<NonZeroU16>,
    maintenance: bool,
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
    let mut selected = match candidate.as_ref().or(previous) {
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
    let shared = super::super::shared::Record::at(guard.root())?;
    let shared_runtime = status != RuntimeStatus::Missing
        && runtime
            .inspect_label(selected.container.as_str(), super::super::shared::LABEL)?
            .as_deref()
            == Some(super::super::shared::VERSION);
    if status == RuntimeStatus::Running && (shared.is_none() || !shared_runtime) {
        return Err(Error::IncompatibleService {
            container: selected.container.as_str().into(),
        });
    }
    if previous.is_some() && (shared.is_none() || status != RuntimeStatus::Running) && !maintenance
    {
        return Err(Error::CellerUnavailable {
            container: selected.container.as_str().into(),
            status,
        });
    }
    super::super::shared::check_inventory(runtime, Some(&selected), None)?;
    let layout = Layout::current()?;
    let issuer = root.join("host-control/celler-issuer");
    if previous.is_some() || candidate.is_some() {
        Authority::open(&issuer).map_err(super::super::cache::Error::from)?;
    }
    layout.prepare()?;
    Authority::provision(&issuer, layout.metadata()).map_err(super::super::cache::Error::from)?;
    if status == RuntimeStatus::Running {
        if maintenance
            && unix_endpoint(root)?
            && !crate::dolt::server::socket::ready(&root.join("service-data/dolt"))
        {
            backend_operation(runtime, &selected, "stop")?;
            crate::dolt::server::socket::retire(&root.join("service-data/dolt"))?;
            backend_operation(runtime, &selected, "start")?;
        }
        ready_all(runtime, &selected, root, &issuer)?;
        if candidate.is_some() {
            commit(&selected, previous, guard)?;
            fs::remove_file(candidate_root.join("celler-owner.json"))?;
        }
        return Ok(selected);
    }
    if shared_runtime && status == RuntimeStatus::Stopped {
        if unix_endpoint(root)? {
            crate::dolt::server::socket::retire(&root.join("service-data/dolt"))?;
        }
        let output = Command::new(&runtime.binary)
            .args(["start", selected.container.as_str()])
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        ready_all(runtime, &selected, root, &issuer)?;
        if candidate.is_some() {
            commit(&selected, previous, guard)?;
            fs::remove_file(candidate_root.join("celler-owner.json"))?;
        }
        return Ok(selected);
    }
    if status == RuntimeStatus::Stopped {
        runtime.remove(&selected.container)?;
    }
    let host = wrix_state::host::Host::discover().map_err(std::io::Error::other)?;
    let public = super::super::generation::controller(&host)?;
    let shared = super::super::shared::Record::prepare_native(guard, public[0], public[4])?;
    if previous.is_none() && candidate.is_none() {
        selected.port = public[1];
    }
    if selected.port != public[1]
        || shared
            .public_sql_port()
            .is_some_and(|port| port != public[0])
    {
        return Err(Error::Operation { message: "stable retained role endpoints were not bound; free them and repeat explicit host maintenance before opening backend data".into() });
    }
    let data = root.join("service-data/dolt");
    crate::dolt::private_directory(&data)?;
    wrix_cache::backend::layout::runtime::install(layout.metadata())?;
    let resumed_runtime = Runtime {
        binary: runtime.binary.clone(),
        kind: runtime.kind,
        binding: if maintenance {
            runtime.binding.clone()
        } else {
            selected
                .recipe
                .as_ref()
                .map(|recipe| recipe.binding.clone())
                .or_else(|| runtime.binding.clone())
        },
    };
    let image = resumed_runtime.ensure_image_at(root)?;
    let recovered = Selected {
        workspace: None,
        recipe: Some(image.clone_recipe()),
        ..selected.clone()
    };
    wrix_core::private_directory::prepare_with_parents(&candidate_root)?;
    recovered.persist(&candidate_root)?;
    let boot = crate::gateway::Secret::generate().map_err(std::io::Error::other)?;
    let mut command = Command::new(&runtime.binary);
    command
        .args(["run", "-d", "--name", selected.container.as_str()])
        .arg("--label")
        .arg(format!("wrix.service.boot={}", boot.expose()))
        .args(["-e", "WRIX_BACKEND_BOOT"])
        .env("WRIX_BACKEND_BOOT", boot.expose());
    if runtime.kind == RuntimeKind::Podman {
        command.args(["--restart=no", "--pull=never"]);
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
        .arg(format!(
            "127.0.0.1:{}:8080",
            shared.cache_port(selected.port)
        ))
        .arg("--label")
        .arg(format!(
            "wrix.cache.native-port={}",
            shared.cache_port(selected.port)
        ))
        .args(["-e", "WRIX_CELLER_DRAIN_SOCKET=/run/wrix/celler/drain.sock"]);
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
    let compiler = root.join("service-data/compiler");
    crate::dolt::private_directory(&compiler)?;
    command
        .arg("-v")
        .arg(format!("{}:{}:rw", compiler.display(), compiler.display()));
    command.args(shared.arguments(&data));
    let output = command
        .arg(image.as_str())
        .arg("wrix-shared-service")
        .arg(&data)
        .arg(if shared.native_ports().is_some() {
            "tcp"
        } else {
            shared.transport()
        })
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(
            super::super::bind_port_from_runtime_error(&message).map_or_else(
                || Error::Operation { message },
                |port| runtime.port_in_use_error(port),
            ),
        );
    }
    ready_all(runtime, &recovered, root, &issuer)?;
    commit(&recovered, previous, guard)?;
    fs::remove_file(candidate_root.join("celler-owner.json"))?;
    Ok(recovered)
}

fn ready_all(runtime: &Runtime, selected: &Selected, root: &Path, issuer: &Path) -> Result<()> {
    ready(
        root,
        issuer,
        runtime.native_cache_port(&selected.container, selected.port)?,
    )?;
    sql_ready(runtime, selected, root)?;
    if let Some((sql, cache, compiler, sync)) =
        super::super::shared::Record::at(&root.join("host-lifecycle"))?
            .and_then(|record| record.native_ports())
    {
        super::super::generation::ensure(selected, sql, cache, compiler, sync)?;
    }
    Ok(())
}

fn unix_endpoint(root: &Path) -> Result<bool> {
    let record = super::super::shared::Record::at(&root.join("host-lifecycle"))?
        .ok_or(super::Error::Invalid)?;
    Ok(matches!(
        record.endpoint(root),
        super::super::DoltEndpoint::UnixSocket { .. }
    ))
}

fn backend_operation(runtime: &Runtime, selected: &Selected, operation: &str) -> Result<()> {
    let mut command = Command::new(&runtime.binary);
    command.arg(operation);
    if operation == "stop" {
        command.args(["--time", "30"]);
    }
    let output = command.arg(selected.container.as_str()).output()?;
    if !output.status.success() {
        return Err(Error::Operation {
            message: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

pub(in crate::lifecycle) fn sql_ready(
    runtime: &Runtime,
    selected: &Selected,
    root: &Path,
) -> Result<()> {
    let unix = unix_endpoint(root)?;
    let native = super::super::shared::Record::at(&root.join("host-lifecycle"))?
        .is_some_and(|record| record.native_ports().is_some());
    let port = if native { "13306" } else { "3306" };
    let namespaces = crate::control::required_sql(root).map_err(std::io::Error::other)?;
    let credentials = crate::dolt::readiness_credentials(root, &namespaces)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let data = root.join("service-data/dolt");
        let output = Command::new("timeout").args(["--signal=KILL", "2"]).arg(&runtime.binary)
            .args(["exec", selected.container.as_str(), "sh", "-c", "export DOLT_CLI_PASSWORD=\"$(cat -- \"$1/admin.password\")\"; exec dolt --host 127.0.0.1 --port \"$2\" --no-tls --user wrixadmin --use-db controller sql -r json -q 'SELECT @@GLOBAL.secure_file_priv AS secure_file_priv'", "wrix-ready"])
            .arg(&data).arg(port).output()?;
        if output.status.success() {
            let value: serde_json::Value =
                serde_json::from_slice(&output.stdout).map_err(std::io::Error::other)?;
            if value["rows"][0]["secure_file_priv"] != "/dev/null" {
                return Err(crate::dolt::Error::UnsafeServer.into());
            }
            for credentials in &credentials {
                let output = Command::new("timeout")
                    .args(["--signal=KILL", "2"])
                    .arg(&runtime.binary)
                    .args(["exec", "-e"])
                    .arg(format!("DOLT_CLI_PASSWORD={}", credentials.password()))
                    .args([
                        selected.container.as_str(),
                        "dolt",
                        "--host",
                        "127.0.0.1",
                        "--port",
                        port,
                        "--no-tls",
                        "--user",
                        credentials.user(),
                        "--use-db",
                        credentials.database(),
                        "sql",
                        "-r",
                        "json",
                        "-q",
                        "SELECT DATABASE() AS wrix_namespace",
                    ])
                    .output()?;
                if !output.status.success() {
                    return Err(crate::dolt::Error::Unauthorized.into());
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&output.stdout).map_err(std::io::Error::other)?;
                if value["rows"].as_array().map(Vec::len) != Some(1)
                    || value["rows"][0]["wrix_namespace"] != credentials.database()
                {
                    return Err(crate::dolt::Error::Unauthorized.into());
                }
            }
            if native {
                let output = Command::new(&runtime.binary)
                    .args([
                        "exec",
                        selected.container.as_str(),
                        "wrix-shared-service",
                        "sql-drain",
                        "abort",
                    ])
                    .output()?;
                if !output.status.success() {
                    return Err(crate::dolt::Error::Incomplete.into());
                }
                let response: super::super::drain::Response =
                    serde_json::from_slice(&output.stdout).map_err(std::io::Error::other)?;
                if !response.accepted
                    || response.status.phase != super::super::drain::Phase::Serving
                {
                    return Err(crate::dolt::Error::Incomplete.into());
                }
            }
            if !unix || crate::dolt::server::socket::ready(&data) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(crate::dolt::Error::SocketUnavailable {
                    path: data.join("run/dolt.sock"),
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if std::time::Instant::now() >= deadline {
            return Err(crate::dolt::Error::Native {
                stderr: "authenticated native Dolt readiness did not complete".into(),
            }
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn commit(
    selected: &Selected,
    previous: Option<&Selected>,
    guard: &coordination::Guard,
) -> Result<()> {
    if Selected::read(guard.root())?.as_ref() != previous {
        return Err(super::Error::Conflict.into());
    }
    match previous {
        None => selected.record(guard)?,
        Some(previous) if previous.workspace == selected.workspace => {
            selected.recovered(previous, guard)?;
        }
        Some(_) => selected.persist(guard.root())?,
    }
    Ok(())
}

pub(in crate::lifecycle) fn validate(
    runtime: &Runtime,
    owner: &Selected,
    status: RuntimeStatus,
) -> Result<()> {
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
            && !runtime.published_ports(owner.container.as_str())?.contains(
                &runtime
                    .native_cache_port(&owner.container, owner.port)?
                    .get(),
            ))
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

pub(in crate::lifecycle) fn ready(root: &Path, issuer: &Path, port: NonZeroU16) -> Result<()> {
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
