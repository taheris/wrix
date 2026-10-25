//! Host-only Podman lifetime reconciliation for exact, durably bound container IDs.

mod containment;
pub use containment::{Prepared, Running};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::Path,
    process::{Command, ExitStatus, Stdio},
};
use wrix_state::{
    context::Context,
    execution::{self, Container, Phase, Ticket},
    host::Host,
};

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// trusted execution state failed: {0}
    State(#[from] wrix_state::store::Error),
    /// host execution capability unavailable: {0}
    Host(#[from] wrix_state::host::Error),
    /// runtime observation failed: {0}
    Io(#[from] io::Error),
    /// runtime returned an ambiguous or unavailable lifetime observation
    Unknown,
    /// native image or read-only delivery differs from the approved worker
    Delivery,
    /// runtime returned malformed containment identity
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Observation {
    Protected,
    Retired,
}

pub fn reserve(workspace: &Path) -> Result<Ticket, Error> {
    let host = Host::discover()?;
    let context = Context::inspect(&host, workspace)?;
    Ok(execution::reserve(&host, &context)?)
}

/// Launch using a pre-reserved ticket, binding the immutable ID before any process starts.
pub fn launch(
    workspace: &Path,
    ticket: &Ticket,
    command: &Command,
    interactive: bool,
) -> Result<ExitStatus, Error> {
    Ok(self::command(workspace, ticket, command, interactive)?.status()?)
}

/// Bind exact containment before starting it; the caller owns waiting/renewal.
pub fn command(
    workspace: &Path,
    ticket: &Ticket,
    command: &Command,
    interactive: bool,
) -> Result<Command, Error> {
    if std::env::var_os("WRIX_UNSAFE_PODMAN_SOCKET").is_some() {
        return Err(Error::Unknown);
    }
    let host = Host::discover()?;
    let context = Context::inspect(&host, workspace)?;
    execution::begin(&host, &context, ticket)?;
    let id = create(command)?;
    execution::bind(&host, &context, ticket, &id)?;
    context.recheck(&host)?;
    Ok(start(command, &id, interactive))
}

fn runtime_command(template: &Command) -> Command {
    let mut command = Command::new(template.get_program());
    for (name, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    if let Some(directory) = template.get_current_dir() {
        command.current_dir(directory);
    }
    command
}

fn create(command: &Command) -> Result<Container, Error> {
    if std::env::var_os("WRIX_UNSAFE_PODMAN_SOCKET").is_some() {
        return Err(Error::Unknown);
    }
    let mut args = command.get_args();
    if args.next() != Some(std::ffi::OsStr::new("run"))
        || args.next() != Some(std::ffi::OsStr::new("--rm"))
    {
        return Err(Error::Unknown);
    }
    let mut create = runtime_command(command);
    create.arg("create").args(args);
    let output = create.stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| Error::Unknown)?;
    let id = Container::parse(text.trim())?;
    Ok(id)
}

fn start(command: &Command, id: &Container, interactive: bool) -> Command {
    let mut start = runtime_command(command);
    start.args(["start", "--attach"]);
    if interactive {
        start.arg("--interactive");
    }
    start
        .arg(id.as_str())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    start
}

/// Read-only projection of one exact ticket-bound object. Never retirement authority.
#[derive(Debug, Serialize, Deserialize)]
pub struct Projection {
    #[serde(rename = "Id")]
    pub id: Container,
    #[serde(rename = "Image", deserialize_with = "deserialize_image")]
    pub image: crate::image::Digest,
    #[serde(rename = "Mounts")]
    pub mounts: Vec<ProjectionMount>,
}

fn deserialize_image<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<crate::image::Digest, D::Error> {
    let value = String::deserialize(deserializer)?;
    let value = if value.starts_with("sha256:") {
        value
    } else {
        format!("sha256:{value}")
    };
    crate::image::Digest::parse(&value).map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionMount {
    #[serde(rename = "Source")]
    pub source: std::path::PathBuf,
    #[serde(rename = "Destination")]
    pub destination: std::path::PathBuf,
    #[serde(rename = "RW")]
    pub writable: bool,
}

pub fn projection(workspace: &Path, ticket: &Ticket) -> Result<Projection, Error> {
    let host = Host::discover()?;
    let context = Context::inspect(&host, workspace)?;
    let Some(Phase::Bound(id)) = execution::observe(&host, &context, ticket)? else {
        return Err(Error::Unknown);
    };
    let projection = projection_for(&Command::new("podman"), &id)?;
    context.recheck(&host)?;
    Ok(projection)
}

fn projection_for(runtime: &Command, id: &Container) -> Result<Projection, Error> {
    let output = runtime_command(runtime)
        .args([
            "inspect",
            "--type",
            "container",
            "--format",
            r#"{"Id":"{{.Id}}","Image":"{{.Image}}","Mounts":{{json .Mounts}}}"#,
            id.as_str(),
        ])
        .output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let projection: Projection = serde_json::from_slice(&output.stdout)?;
    if &projection.id != id {
        return Err(Error::Unknown);
    }
    Ok(projection)
}

/// Missing objects and successful launcher exits do not establish teardown.
pub fn reconcile(workspace: &Path, ticket: &Ticket) -> Result<Observation, Error> {
    let host = Host::discover()?;
    let context = Context::inspect(&host, workspace)?;
    let Some(phase) = execution::observe(&host, &context, ticket)? else {
        return Err(Error::Unknown);
    };
    let id = match phase {
        Phase::Retired(_) => return Ok(Observation::Retired),
        Phase::Reserved | Phase::Cancelled | Phase::Launching => return Ok(Observation::Protected),
        Phase::Bound(id) => id,
    };
    let observation = remove(&Command::new("podman"), &id, || {
        context.recheck(&host)?;
        Ok(())
    })?;
    if observation == Observation::Retired {
        execution::retire(&host, &context, ticket, &id)?;
    }
    Ok(observation)
}

fn remove(
    runtime: &Command,
    id: &Container,
    mut recheck: impl FnMut() -> Result<(), Error>,
) -> Result<Observation, Error> {
    recheck()?;
    let output = runtime_command(runtime)
        .args(["inspect", "--type", "container", id.as_str()])
        .output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let objects: Vec<Inspect> = serde_json::from_slice(&output.stdout)?;
    if !inactive(&objects, id) {
        return Ok(Observation::Protected);
    }
    recheck()?;
    let removed = runtime_command(runtime)
        .args(["rm", id.as_str()])
        .output()?;
    if !removed.status.success() || String::from_utf8_lossy(&removed.stdout).trim() != id.as_str() {
        return Err(Error::Unknown);
    }
    let absent = runtime_command(runtime)
        .args(["container", "exists", id.as_str()])
        .output()?;
    if absent.status.code() != Some(1) || !absent.stdout.is_empty() {
        return Err(Error::Unknown);
    }
    recheck()?;
    Ok(Observation::Retired)
}

#[derive(Deserialize)]
struct Inspect {
    #[serde(rename = "Id")]
    id: Container,
    #[serde(rename = "State")]
    state: State,
}

#[derive(Deserialize)]
struct State {
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "Paused")]
    paused: bool,
    #[serde(rename = "Restarting")]
    restarting: bool,
}

fn inactive(objects: &[Inspect], id: &Container) -> bool {
    matches!(objects, [object] if &object.id == id && object.state.status == "exited"
        && !object.state.running && !object.state.paused && !object.state.restarting)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn projection_requires_complete_native_image_identity_and_canonicalizes_its_digest() {
        for value in ["1".repeat(64), format!("sha256:{}", "1".repeat(64))] {
            let projection: Projection = serde_json::from_value(
                serde_json::json!({"Id":"2".repeat(64),"Image":value,"Mounts":[]}),
            )
            .unwrap();
            assert_eq!(
                projection.image.as_str(),
                format!("sha256:{}", "1".repeat(64))
            );
        }
        for value in ["latest", "1234", "sha256:short"] {
            assert!(
                serde_json::from_value::<Projection>(
                    serde_json::json!({"Id":"2".repeat(64),"Image":value,"Mounts":[]})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn execution_reconciliation_requires_one_exact_inactive_container() {
        let id = Container::parse(&"1".repeat(64)).unwrap();
        let other = Container::parse(&"2".repeat(64)).unwrap();
        for (status, running, paused, restarting, expected) in [
            ("running", true, false, false, false),
            ("created", false, false, false, false),
            ("unknown", false, false, false, false),
            ("exited", true, false, false, false),
            ("exited", false, true, false, false),
            ("exited", false, false, true, false),
            ("exited", false, false, false, true),
        ] {
            let object = Inspect {
                id: id.clone(),
                state: State {
                    status: status.into(),
                    running,
                    paused,
                    restarting,
                },
            };
            assert_eq!(inactive(std::slice::from_ref(&object), &id), expected);
            assert!(!inactive(std::slice::from_ref(&object), &other));
            assert!(!inactive(
                &[
                    object,
                    Inspect {
                        id: id.clone(),
                        state: State {
                            status: status.into(),
                            running,
                            paused,
                            restarting
                        }
                    }
                ],
                &id
            ));
        }
        assert!(!inactive(&[], &id));
    }
}
