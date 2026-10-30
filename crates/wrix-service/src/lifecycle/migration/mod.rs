//! Operator-coordinated legacy cutover; retained containers are recovery evidence.

use super::{
    ContainerInfo, Error, Result, Runtime, RuntimeKind, RuntimeStatus, coordination, owner,
};
use serde::{Deserialize, Serialize};
use std::{
    fmt::Write as _,
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use wrix_core::path::ContainerName;

#[cfg(test)]
mod test;

const JOURNAL: &str = "legacy-migration.json";

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Mode {
    Inventory,
    Prepare,
    Quiesced,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
struct NativeId(String);

impl TryFrom<String> for NativeId {
    type Error = owner::Error;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value))
        } else {
            Err(owner::Error::Invalid)
        }
    }
}
impl From<NativeId> for String {
    fn from(value: NativeId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    source: PathBuf,
    destination: PathBuf,
    #[serde(rename = "RW")]
    writable: bool,
}

fn mounts(runtime: &Runtime, container: &str) -> Result<Vec<Mount>> {
    if runtime.kind != RuntimeKind::Podman {
        return Ok(Vec::new());
    }
    let json = runtime
        .inspect_format(container, "{{json .Mounts}}")?
        .ok_or(owner::Error::Invalid)?;
    serde_json::from_str(&json).map_err(|error| io::Error::other(error).into())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    runtime: RuntimeKind,
    binary: PathBuf,
    container: String,
    workspace: Option<PathBuf>,
    namespace: Option<String>,
    native: Option<NativeId>,
    mounts: Vec<Mount>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    authorized: bool,
    entries: Vec<Entry>,
    consumers: Vec<String>,
    verified: Vec<PathBuf>,
}

fn failure(message: impl Into<String>) -> Error {
    Error::Operation {
        message: message.into(),
    }
}

fn name(entry: &Entry) -> Result<ContainerName> {
    ContainerName::from_persisted(entry.container.clone())
        .ok_or_else(|| owner::Error::Invalid.into())
}

fn backend(entry: &Entry) -> Result<Runtime> {
    Ok(Runtime {
        kind: entry.runtime,
        binary: entry.binary.to_str().ok_or(owner::Error::Invalid)?.into(),
        binding: None,
    })
}

fn read(root: &Path) -> Result<Option<Journal>> {
    let file = match fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(JOURNAL))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != fs::metadata(root)?.uid()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > 1024 * 1024
    {
        return Err(owner::Error::Invalid.into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let journal: Journal = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if journal.version != 1 || bytes.len() > 1024 * 1024 {
        return Err(owner::Error::Invalid.into());
    }
    Ok(Some(journal))
}

fn persist(root: &Path, journal: &Journal) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(journal).map_err(io::Error::other)?)?;
    file.as_file().sync_all()?;
    file.persist(root.join(JOURNAL))
        .map_err(|error| error.error)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn entry(runtime: &Runtime, container: ContainerInfo) -> Result<Entry> {
    let native = if runtime.kind == RuntimeKind::Podman {
        runtime
            .inspect_format(&container.name, "{{.Id}}")?
            .map(NativeId::try_from)
            .transpose()?
    } else {
        None
    };
    Ok(Entry {
        runtime: runtime.kind,
        binary: fs::canonicalize(
            runtime
                .inventory_binary()?
                .ok_or_else(|| failure("runtime disappeared during inventory"))?,
        )?,
        container: container.name.clone(),
        workspace: container.workspace_path.map(PathBuf::from),
        namespace: container.workspace_hash,
        native,
        mounts: mounts(runtime, &container.name)?,
    })
}

fn inventory(runtime: &Runtime, root: &Path) -> Result<(Vec<Entry>, Vec<String>)> {
    inventory_backends(&[&runtime.alternate(), runtime], root)
}

fn inventory_backends(backends: &[&Runtime], root: &Path) -> Result<(Vec<Entry>, Vec<String>)> {
    let selected = owner::Selected::read(root)?;
    let mut entries = Vec::new();
    let mut consumers = Vec::new();
    for &backend in backends {
        if let Some(containers) = backend.available_container_infos()? {
            for container in containers {
                let container_name = ContainerName::from_persisted(container.name.clone())
                    .ok_or(owner::Error::Invalid)?;
                if container.kind.as_deref() != Some("service") {
                    if backend.status(&container_name)? == RuntimeStatus::Running {
                        consumers.push(format!(
                            "runtime={} container={} workspace={:?} mounts={:?}",
                            backend.binary,
                            container.name,
                            container.workspace_path,
                            mounts(backend, &container.name)?
                        ));
                    }
                    continue;
                }
                if selected.as_ref().is_some_and(|owner| {
                    owner.runtime == backend.kind && owner.container == container_name
                }) && backend
                    .inspect_label(&container.name, super::shared::LABEL)?
                    .as_deref()
                    == Some(super::shared::VERSION)
                    || super::generation::owns(
                        root.parent().ok_or(owner::Error::Invalid)?,
                        backend.kind,
                        &container.name,
                    )?
                {
                    continue;
                }
                entries.push(entry(backend, container)?);
            }
        }
    }
    entries.sort_by(|a, b| (&a.binary, &a.container).cmp(&(&b.binary, &b.container)));
    consumers.sort();
    Ok((entries, consumers))
}

pub(super) fn incompatible(runtime: &Runtime, container: &ContainerInfo) -> Result<Error> {
    Ok(failure(format!(
        "running foreign legacy service: workspace={:?} runtime={} container={}; not a verified shared Dolt/Celler owner. Recovery cannot migrate a running foreign service. Retain its database, cache and signing state; coordinate maintenance with all consumers. Run `{}` to inventory and prepare migration. A recurrence after cutover indicates a stale shell/package: update WRIX_BIN, PATH, direnv and launch automation before retrying",
        container.workspace_path,
        runtime.binary,
        container.name,
        migration_command(runtime)?
    )))
}

fn migration_command(runtime: &Runtime) -> Result<String> {
    let executable = std::env::var_os("WRIX_INVOKED_BIN")
        .map_or_else(std::env::current_exe, |value| Ok(PathBuf::from(value)))?;
    let executable = if executable.is_absolute() {
        executable
    } else {
        std::env::current_dir()?.join(executable)
    };
    let binary = runtime.inventory_binary()?.ok_or(owner::Error::Invalid)?;
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"));
    Ok(format!(
        "env WRIX_CONTAINER_RUNTIME={} {} service migrate-legacy",
        quote(&binary),
        quote(&executable)
    ))
}

/// Only an explicitly quiesced, unchanged retained container permits namespace import.
pub(super) fn permits(root: &Path, runtime: &Runtime, container: &ContainerInfo) -> Result<bool> {
    let Some(journal) = read(root)? else {
        return Ok(false);
    };
    if !journal.authorized {
        return Ok(false);
    }
    let current = entry(runtime, container.clone())?;
    Ok(journal.entries.contains(&current)
        && runtime.status(&name(&current)?)? == RuntimeStatus::Stopped)
}

fn validate_quiescent(entries: &[Entry]) -> Result<()> {
    for item in entries {
        let runtime = backend(item)?;
        if runtime.status(&name(item)?)? != RuntimeStatus::Stopped {
            return Err(failure(format!(
                "maintenance approval required: workspace={:?} runtime={} container={} must be gracefully stopped by its operator after all SQL/cache clients and old launchers are quiesced; no service was stopped by migration",
                item.workspace,
                item.binary.display(),
                item.container
            )));
        }
        if item.runtime != RuntimeKind::Podman || item.native.is_none() {
            return Err(failure(
                "this runtime lacks a supported immutable container identity for legacy cutover; retained state is untouched; use a verified runtime-specific maintenance procedure",
            ));
        }
        let workspace = item.workspace.as_ref().ok_or_else(|| failure("legacy service has no workspace label; reconcile its mounts manually before cutover"))?;
        if fs::canonicalize(workspace)? != *workspace {
            return Err(failure(
                "legacy workspace label is not canonical; reconcile its exact checkout before cutover",
            ));
        }
    }
    Ok(())
}

fn verify_source_retired(workspace: &Path) -> Result<()> {
    let metadata: serde_json::Value = match fs::read(workspace.join(".beads/metadata.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => return Err(error.into()),
    };
    let mut source = workspace.join(".beads/dolt");
    if let Some(database) = metadata.get("dolt_database") {
        let database = database.as_str().ok_or(owner::Error::Invalid)?;
        if database.is_empty()
            || database.len() > 128
            || database == "."
            || database == ".."
            || database.contains('/')
        {
            return Err(owner::Error::Invalid.into());
        }
        source.push(database);
    }
    let mut pending = source.join(".dolt/noms").try_exists()?;
    if metadata.get("dolt_database").is_none() && source.try_exists()? {
        for child in fs::read_dir(&source)? {
            let child = child?;
            if child.file_type()?.is_symlink() {
                return Err(owner::Error::Invalid.into());
            }
            pending |= child.path().join(".dolt/noms").try_exists()?;
        }
    }
    if pending {
        return Err(failure(format!(
            "legacy database still exists at {}; the shared namespace may already contain independent history. Both copies are retained; migration is incomplete. Reconcile their history under maintenance before rerunning --quiesced; never overwrite either database or rotate cache keys as a workaround",
            source.display()
        )));
    }
    Ok(())
}

fn invoke(workspace: &Path, runtime: &Runtime, args: &[&str]) -> Result<()> {
    let executable = std::env::var_os("WRIX_INVOKED_BIN")
        .map_or_else(std::env::current_exe, |value| Ok(PathBuf::from(value)))?;
    if !executable.is_absolute() {
        return Err(failure(
            "migration requires an absolute invoked host executable",
        ));
    }
    let output = Command::new(executable)
        .args(args)
        .current_dir(workspace)
        .env("WRIX_CONTAINER_RUNTIME", &runtime.binary)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(failure(format!(
            "migration verification failed in {} for {args:?}: {}{}. Retained containers, cache/signing state and migration journal were not deleted. Resolve the error and repeat `wrix service migrate-legacy --quiesced`; do not rotate keys or reset databases to bypass it",
            workspace.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

fn admit_cutover(journal: &Journal, entries: &[Entry], consumers: &[String]) -> Result<()> {
    if entries != journal.entries {
        return Err(failure(
            "legacy inventory or immutable identity changed; no cutover authorized; retain containers and the migration journal for reconciliation",
        ));
    }
    if !consumers.is_empty() {
        return Err(failure(format!(
            "active possible consumers block cutover: {}; quiesce them and host SQL/cache clients before retrying",
            consumers.join("; ")
        )));
    }
    validate_quiescent(entries)
}

/// Inventory is passive; prepare records intent; quiesced acknowledges operator downtime.
pub fn run(mode: Mode) -> Result<String> {
    let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
    let root = host.roots().state().join("host-lifecycle");
    let runtime = Runtime::from_env()?;
    let _migration = coordination::acquire(&host.roots().state().join("legacy-migration"))?;
    let guard = coordination::acquire(&root)?;
    let (entries, consumers) = inventory(&runtime, &root)?;
    let saved = read(&root)?;
    if mode != Mode::Quiesced {
        let mut report = String::from("Legacy migration inventory (no services stopped):\n");
        for item in &entries {
            writeln!(report,
                "workspace={:?} runtime={} container={} identity={:?} status={:?} retained_mounts={:?}",
                item.workspace,
                item.binary.display(),
                item.container,
                item.native,
                backend(item)?.status(&name(item)?)?, item.mounts
            ).map_err(io::Error::other)?;
        }
        for consumer in &consumers {
            writeln!(report, "possible consumer: {consumer}").map_err(io::Error::other)?;
        }
        if let Some(journal) = &saved {
            writeln!(
                report,
                "Journal: {} authorized={} verified={:?}",
                root.join(JOURNAL).display(),
                journal.authorized,
                journal.verified
            )
            .map_err(io::Error::other)?;
        }
        if mode == Mode::Prepare {
            if let Some(journal) = saved {
                if journal.entries != entries {
                    return Err(failure(
                        "inventory changed since migration preparation; retain the journal and reconcile new or missing services before proceeding",
                    ));
                }
            } else {
                persist(
                    &root,
                    &Journal {
                        version: 1,
                        authorized: false,
                        entries,
                        consumers,
                        verified: Vec::new(),
                    },
                )?;
            }
            report.push_str("Durable intent retained.\n");
        }
        report.push_str("Host shell/SQL/cache consumers cannot be proven idle by container inventory. Obtain maintenance approval for every listed workspace, quiesce consumers and old launchers, then gracefully stop only the listed legacy containers (never remove them or their volumes). Retain all .beads databases, native state/cache roots and signing files. Update WRIX_BIN/PATH, project pins and automation; reload stale shells. Run `wrix service migrate-legacy --prepare` before shutdown, then `wrix service migrate-legacy --quiesced` to resume/import and verify every workspace. Any running non-service container conservatively blocks cutover. Retained legacy cache views are not merged or key-rotated.\n");
        return Ok(report);
    }
    let mut journal = saved.ok_or_else(|| {
        failure("run `wrix service migrate-legacy --prepare` before coordinated shutdown")
    })?;
    admit_cutover(&journal, &entries, &consumers)?;
    if entries.is_empty() {
        return Ok("No foreign legacy services to migrate.\n".into());
    }
    let selected = owner::Selected::read(&root)?.ok_or_else(|| {
        failure("foreign-service migration requires an existing verified shared owner")
    })?;
    if entries.iter().any(|entry| {
        entry.runtime == selected.runtime && entry.container == selected.container.as_str()
    }) {
        return Err(failure(
            "the selected owner itself is legacy; foreign-service cutover cannot replace it while retaining its container. Obtain separate selected-owner maintenance approval; no state was changed",
        ));
    }
    if runtime.kind != selected.runtime {
        return Err(failure(
            "select the shared owner's recorded runtime with WRIX_CONTAINER_RUNTIME before cutover; legacy runtimes remain independently inventoried",
        ));
    }
    let recover = runtime.status(&selected.container)? != RuntimeStatus::Running;
    journal.authorized = true;
    persist(&root, &journal)?;
    drop(guard);
    if recover {
        super::recover()?;
    }
    let mut workspaces: Vec<_> = entries
        .iter()
        .filter_map(|item| item.workspace.clone())
        .collect();
    workspaces.sort();
    workspaces.dedup();
    for workspace in workspaces {
        invoke(&workspace, &runtime, &["service", "start"])?;
        invoke(&workspace, &runtime, &["service", "dolt", "wait"])?;
        invoke(&workspace, &runtime, &["service", "cache", "status"])?;
        verify_source_retired(&workspace)?;
        if !journal.verified.contains(&workspace) {
            journal.verified.push(workspace);
        }
        let _guard = coordination::acquire(&root)?;
        persist(&root, &journal)?;
    }
    let _guard = coordination::acquire(&root)?;
    let (after, consumers) = inventory(&runtime, &root)?;
    if !consumers.is_empty() {
        return Err(failure(
            "consumers appeared during cutover; verification is incomplete; quiesce clients and rerun --quiesced",
        ));
    }
    if after != journal.entries {
        return Err(failure(
            "legacy launch recurrence detected during cutover; update stale launchers, retain state and repeat inventory",
        ));
    }
    validate_quiescent(&after)?;
    Ok(format!(
        "Verified {} workspace(s). Legacy containers and all cache/signing state retained; journal: {}. Do not restart legacy services. Unselected sibling databases remain retained, not implicitly merged.\n",
        journal.verified.len(),
        root.join(JOURNAL).display()
    ))
}
