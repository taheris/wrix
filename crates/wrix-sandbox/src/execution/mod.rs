//! Host-only Podman lifetime reconciliation for exact, durably bound container IDs.

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
    let mut args = command.get_args();
    if args.next() != Some(std::ffi::OsStr::new("run"))
        || args.next() != Some(std::ffi::OsStr::new("--rm"))
    {
        return Err(Error::Unknown);
    }
    let mut create = Command::new(command.get_program());
    create.arg("create").args(args);
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            create.env(name, value);
        } else {
            create.env_remove(name);
        }
    }
    if let Some(directory) = command.get_current_dir() {
        create.current_dir(directory);
    }
    let output = create.stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| Error::Unknown)?;
    let id = Container::parse(text.trim())?;
    execution::bind(&host, &context, ticket, &id)?;
    context.recheck(&host)?;
    let mut start = Command::new(command.get_program());
    start.args(["start", "--attach"]);
    if interactive {
        start.arg("--interactive");
    }
    start
        .arg(id.as_str())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(start)
}

/// Read-only projection of one exact ticket-bound object. Never retirement authority.
#[derive(Debug, Serialize, Deserialize)]
pub struct Projection {
    #[serde(rename = "Id")]
    pub id: Container,
    #[serde(rename = "Mounts")]
    pub mounts: Vec<ProjectionMount>,
}

#[derive(Debug, Serialize, Deserialize)]
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
    let output = Command::new("podman")
        .args([
            "inspect",
            "--type",
            "container",
            "--format",
            r#"{"Id":"{{.Id}}","Mounts":{{json .Mounts}}}"#,
            id.as_str(),
        ])
        .output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let projection: Projection = serde_json::from_slice(&output.stdout)?;
    if projection.id != id {
        return Err(Error::Unknown);
    }
    context.recheck(&host)?;
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
        Phase::Reserved | Phase::Launching => return Ok(Observation::Protected),
        Phase::Bound(id) => id,
    };
    let output = Command::new("podman")
        .args(["inspect", "--type", "container", id.as_str()])
        .output()?;
    if !output.status.success() {
        return Err(Error::Unknown);
    }
    let objects: Vec<Inspect> = serde_json::from_slice(&output.stdout)?;
    if !inactive(&objects, &id) {
        return Ok(Observation::Protected);
    }
    context.recheck(&host)?;
    let removed = Command::new("podman").args(["rm", id.as_str()]).output()?;
    if !removed.status.success() || String::from_utf8_lossy(&removed.stdout).trim() != id.as_str() {
        return Err(Error::Unknown);
    }
    execution::retire(&host, &context, ticket, &id)?;
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
