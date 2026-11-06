use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    process::ExitCode,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported/mutating runtime command
    Usage,
    /// runtime fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// runtime fixture JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// Error: no such container
    Missing,
    /// unsupported inspection: {0}
    Inspection(String),
    /// invalid fixture container field: {0}
    Field(&'static str),
}

fn inspect(
    containers: &BTreeMap<String, Value>,
    template: &str,
    name: &str,
) -> Result<String, Error> {
    let item = containers.get(name).ok_or(Error::Missing)?;
    match template {
        "{{.State.Running}}" => Ok(item["running"]
            .as_bool()
            .ok_or(Error::Field("running"))?
            .to_string()),
        "{{.Id}}" => Ok(item["id"].as_str().ok_or(Error::Field("id"))?.into()),
        "{{json .Mounts}}" => Ok(serde_json::to_string(&item["mounts"])?),
        _ => {
            let label = template
                .strip_prefix("{{ index .Config.Labels \"")
                .and_then(|value| value.strip_suffix("\" }}"))
                .ok_or_else(|| Error::Inspection(template.into()))?;
            Ok(item["labels"][label]
                .as_str()
                .unwrap_or("<no value>")
                .into())
        }
    }
}

fn response(containers: &BTreeMap<String, Value>, args: &[OsString]) -> Result<String, Error> {
    let words = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Error::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    match words.as_slice() {
        ["ps", "-a", "--format", "{{.Names}}"] => {
            Ok(containers.keys().cloned().collect::<Vec<_>>().join("\n"))
        }
        ["port", _] => Ok(String::new()),
        ["inspect", "--format", template, name, "--type=container"] => {
            inspect(containers, template, name)
        }
        _ => Err(Error::Usage),
    }
}

pub(super) fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    let Some((root, args)) = args.split_first() else {
        return Err(Error::Usage);
    };
    let root = Path::new(root);
    let containers = serde_json::from_slice(&fs::read(root.join("runtime.json"))?)?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("calls"))?;
    let arguments = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Error::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_writer(&mut log, &arguments)?;
    log.write_all(b"\n")?;
    let output = response(&containers, args)?;
    if !output.is_empty() {
        writeln!(io::stdout().lock(), "{output}")?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;

    fn containers() -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "running".into(),
                json!({"running":true,"id":"a","labels":{"wrix.kind":"service"},"mounts":[{"Source":"/owned","RW":true}]}),
            ),
            (
                "stopped".into(),
                json!({"running":false,"id":"b","labels":{},"mounts":[]}),
            ),
        ])
    }

    #[test]
    fn inventory_reports_both_states_ids_labels_and_mounts() {
        let containers = containers();
        assert_eq!(
            inspect(&containers, "{{.State.Running}}", "running").unwrap(),
            "true"
        );
        assert_eq!(
            inspect(&containers, "{{.State.Running}}", "stopped").unwrap(),
            "false"
        );
        assert_eq!(inspect(&containers, "{{.Id}}", "running").unwrap(), "a");
        assert_eq!(
            inspect(
                &containers,
                "{{ index .Config.Labels \"wrix.kind\" }}",
                "running"
            )
            .unwrap(),
            "service"
        );
        assert_eq!(
            inspect(
                &containers,
                "{{ index .Config.Labels \"wrix.kind\" }}",
                "stopped"
            )
            .unwrap(),
            "<no value>"
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &inspect(&containers, "{{json .Mounts}}", "running").unwrap()
            )
            .unwrap(),
            containers["running"]["mounts"]
        );
    }

    #[test]
    fn inventory_refuses_missing_containers_unknown_inspections_and_mutations() {
        let containers = containers();
        assert!(matches!(
            inspect(&containers, "{{.Id}}", "missing"),
            Err(Error::Missing)
        ));
        assert!(matches!(
            inspect(&containers, "unknown", "running"),
            Err(Error::Inspection(_))
        ));
        for operation in ["stop", "start", "rm", "exec", "run"] {
            assert!(matches!(
                response(&containers, &[operation, "running"].map(OsString::from)),
                Err(Error::Usage)
            ));
        }
        assert_eq!(
            response(
                &containers,
                &["ps", "-a", "--format", "{{.Names}}"].map(OsString::from)
            )
            .unwrap(),
            "running\nstopped"
        );
        assert_eq!(
            response(&containers, &["port", "running"].map(OsString::from)).unwrap(),
            ""
        );
    }
}
