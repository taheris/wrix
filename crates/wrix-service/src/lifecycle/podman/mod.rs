//! Positive local-machine evidence; an unreachable or remote engine is never an empty inventory.
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct Machine {
    name: String,
    running: bool,
    starting: bool,
    port: u16,
    remote_username: String,
    identity_path: PathBuf,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct Connection {
    name: String,
    #[serde(rename = "URI")]
    uri: String,
    identity: PathBuf,
    is_machine: bool,
    default: bool,
}

fn proves_stopped(machines: &[u8], connections: &[u8], uid: u32) -> bool {
    let Ok(machines) = serde_json::from_slice::<Vec<Machine>>(machines) else {
        return false;
    };
    let Ok(connections) = serde_json::from_slice::<Vec<Connection>>(connections) else {
        return false;
    };
    if machines.is_empty()
        || connections.is_empty()
        || machines
            .iter()
            .map(|machine| &machine.name)
            .collect::<BTreeSet<_>>()
            .len()
            != machines.len()
        || connections
            .iter()
            .filter(|connection| connection.default)
            .count()
            != 1
        || machines
            .iter()
            .any(|machine| machine.running || machine.starting || machine.port == 0)
    {
        return false;
    }
    connections.iter().all(|connection| {
        connection.is_machine
            && machines.iter().any(|machine| {
                if !machine.identity_path.is_absolute()
                    || connection.identity != machine.identity_path
                {
                    return false;
                }
                let endpoint = if connection.name == machine.name {
                    format!(
                        "ssh://{}@127.0.0.1:{}/run/user/{uid}/podman/podman.sock",
                        machine.remote_username, machine.port
                    )
                } else if connection.name == format!("{}-root", machine.name) {
                    format!(
                        "ssh://root@127.0.0.1:{}/run/podman/podman.sock",
                        machine.port
                    )
                } else {
                    return false;
                };
                connection.uri == endpoint
            })
    })
}

fn query(binary: &Path, args: &[&str]) -> io::Result<Option<Vec<u8>>> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() && output.stdout.len() <= 65536 {
        Ok(Some(output.stdout))
    } else {
        Ok(None)
    }
}

pub(super) fn stopped_local_machine(binary: &Path) -> io::Result<bool> {
    if [
        "CONTAINER_HOST",
        "CONTAINER_CONNECTION",
        "CONTAINER_SSHKEY",
        "CONTAINERS_CONF",
        "PODMAN_CONNECTIONS_CONF",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        return Ok(false);
    }
    let machines = ["machine", "list", "--format", "json"];
    let connections = ["system", "connection", "list", "--format", "json"];
    let Some(first_machines) = query(binary, &machines)? else {
        return Ok(false);
    };
    let Some(first_connections) = query(binary, &connections)? else {
        return Ok(false);
    };
    if !proves_stopped(
        &first_machines,
        &first_connections,
        rustix::process::geteuid().as_raw(),
    ) {
        return Ok(false);
    }
    Ok(query(binary, &machines)?.as_ref() == Some(&first_machines)
        && query(binary, &connections)?.as_ref() == Some(&first_connections))
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::{Value, json};

    fn evidence() -> (Value, Value) {
        (
            json!([{"Name":"local", "Running":false, "Starting":false, "Port":53218,
                 "RemoteUsername":"core", "IdentityPath":"/fixture/key"}]),
            json!([{"Name":"local", "URI":"ssh://core@127.0.0.1:53218/run/user/501/podman/podman.sock",
                 "Identity":"/fixture/key", "IsMachine":true, "Default":true},
                {"Name":"local-root", "URI":"ssh://root@127.0.0.1:53218/run/podman/podman.sock",
                 "Identity":"/fixture/key", "IsMachine":true, "Default":false}]),
        )
    }
    fn proven(machines: &Value, connections: &Value) -> bool {
        proves_stopped(
            &serde_json::to_vec(machines).unwrap(),
            &serde_json::to_vec(connections).unwrap(),
            501,
        )
    }
    #[test]
    fn stopped_local_machine_includes_both_native_root_and_rootless_connections() {
        let (machines, connections) = evidence();
        assert!(proven(&machines, &connections));
    }
    #[test]
    fn running_starting_and_incomplete_machine_evidence_cannot_authorize_an_empty_inventory() {
        for (field, value) in [
            ("Running", json!(true)),
            ("Starting", json!(true)),
            ("Port", json!(0)),
        ] {
            let (mut machines, connections) = evidence();
            machines[0][field] = value;
            assert!(!proven(&machines, &connections));
        }
        let (mut machines, connections) = evidence();
        machines[0].as_object_mut().unwrap().remove("Running");
        assert!(!proven(&machines, &connections));
        assert!(!proven(&json!([]), &connections));
    }
    #[test]
    fn remote_mismatched_and_ambiguous_connections_remain_uncertain() {
        for (field, value) in [
            (
                "URI",
                json!("ssh://core@remote:53218/run/user/501/podman/podman.sock"),
            ),
            ("Identity", json!("/other/key")),
            ("IsMachine", json!(false)),
            ("Default", json!(false)),
        ] {
            let (machines, mut connections) = evidence();
            connections[0][field] = value;
            assert!(!proven(&machines, &connections));
        }
        let (machines, mut connections) = evidence();
        connections[1]["Default"] = json!(true);
        assert!(!proven(&machines, &connections));
    }
}
