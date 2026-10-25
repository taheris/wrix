//! External runtime failure controls over actual private state; actual Podman evidence lives in the native pool app.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    sync::Arc,
};
use wrix_sandbox::execution::{
    Error, Observation,
    retirement::{self, Operation, Runtime},
};
use wrix_state::{
    context::Context,
    execution::{self, Container, Recorded, Removal, Ticket},
    host::Host,
    store::Store,
};

fn child(root: &Path, action: &str) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "retirement_fixture_child", "--nocapture"])
        .env("WRIX_RETIREMENT_ROOT", root)
        .env("WRIX_RETIREMENT_ACTION", action)
        .env("XDG_STATE_HOME", root.join("state"));
    Ok(command)
}
fn run(root: &Path, action: &str) -> std::io::Result<()> {
    let output = child(root, action)?.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
struct External<'a> {
    root: &'a Path,
}
impl Runtime for External<'_> {
    fn query(&mut self, operation: Operation<'_>) -> Result<Output, Error> {
        let mut command = Command::new(self.root.join("runtime"));
        match operation {
            Operation::Inspect(id) => {
                command.args(["inspect", "--type", "container", id.as_str()]);
            }
            Operation::Remove(id) => {
                command.args(["rm", id.as_str()]);
            }
            Operation::Exists(id) => {
                command.args(["container", "exists", id.as_str()]);
            }
        }
        Ok(command.output()?)
    }
}
#[test]
fn retirement_fixture_child() {
    let Some(root) = std::env::var_os("WRIX_RETIREMENT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let action = std::env::var("WRIX_RETIREMENT_ACTION").unwrap();
    let host = Arc::new(Host::discover().unwrap());
    if action == "initialize" {
        Store::initialize(&host).unwrap();
        Store::acknowledge(&host, 1).unwrap();
        let context = Context::inspect(&host, &root).unwrap();
        let ticket = execution::reserve(&host, &context).unwrap();
        execution::begin(&host, &context, &ticket).unwrap();
        execution::bind(
            &host,
            &context,
            &ticket,
            &Container::parse(&"1".repeat(64)).unwrap(),
        )
        .unwrap();
        fs::write(root.join("ticket"), ticket.as_str()).unwrap();
        return;
    }
    let context = Context::inspect(&host, &root).unwrap();
    let ticket = Ticket::parse(&fs::read_to_string(root.join("ticket")).unwrap()).unwrap();
    let generation = Store::inspect(&host)
        .unwrap()
        .unwrap()
        .generation()
        .unwrap();
    let record = Recorded::load(host.clone(), context.key(), &ticket, &generation)
        .unwrap()
        .unwrap();
    let mut runtime = External { root: &root };
    match action.as_str() {
        "bound-unknown" => {
            assert!(retirement::reconcile(&record, &mut runtime).is_err());
            assert_eq!(record.status().unwrap(), Removal::Bound);
        }
        "removing-unknown" => {
            assert!(retirement::reconcile(&record, &mut runtime).is_err());
            assert_eq!(record.status().unwrap(), Removal::Removing);
        }
        "retired" => {
            assert_eq!(
                retirement::reconcile(&record, &mut runtime).unwrap(),
                Observation::Retired
            );
            assert!(
                execution::terminal(&host, context.key(), &ticket)
                    .unwrap()
                    .is_some()
            );
            return;
        }
        "protected" => {
            record.removing().unwrap();
            assert_eq!(
                retirement::reconcile(&record, &mut runtime).unwrap(),
                Observation::Protected
            );
        }
        _ => panic!("unknown action {action}"),
    }
    assert!(
        execution::terminal(&host, context.key(), &ticket)
            .unwrap()
            .is_none()
    );
}
fn script(root: &Path, body: &str) -> std::io::Result<()> {
    let path = root.join("runtime");
    fs::write(
        &path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
fn inspection(status: &str, running: bool) -> String {
    serde_json::json!([{"Id":"1".repeat(64),"State":{"Status":status,"Running":running,"Paused":false,"Restarting":false}}]).to_string()
}
#[test]
#[ignore = "requires host capability; run on the native Linux host"]
fn failed_removal_intent_recovers_after_reader_restart_only_with_confirmed_absence() {
    for (status, reply) in [(125, ""), (0, "wrong-id")] {
        let root = tempfile::tempdir().unwrap();
        run(root.path(), "initialize").unwrap();
        let body = format!(
            "case \"$1\" in\ninspect) printf '%s' '{}' ;;\nrm) touch '{}'; printf '%s' '{reply}'; exit {status} ;;\ncontainer) [[ -f '{}' ]] && exit 1; exit 0 ;;\n*) exit 125 ;;\nesac",
            inspection("exited", false),
            root.path().join("removed").display(),
            root.path().join("removed").display()
        );
        script(root.path(), &body).unwrap();
        run(root.path(), "removing-unknown").unwrap();
        run(root.path(), "retired").unwrap();
    }
}
#[test]
#[ignore = "requires host capability; run on the native Linux host"]
fn missing_objects_without_intent_and_ambiguous_absence_never_retire() {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), "initialize").unwrap();
    script(
        root.path(),
        "case \"$1\" in inspect) exit 125 ;; container) exit 1 ;; *) exit 125 ;; esac",
    )
    .unwrap();
    run(root.path(), "bound-unknown").unwrap();
    for (status, reply) in [(0, ""), (125, ""), (1, "unexpected")] {
        script(
            root.path(),
            &format!(
                "case \"$1\" in inspect) printf '%s' '{}' ;; rm) printf '%s' '{}' ;; container) printf '%s' '{reply}'; exit {status} ;; *) exit 125 ;; esac",
                inspection("exited", false),
                "1".repeat(64)
            ),
        ).unwrap();
        run(root.path(), "removing-unknown").unwrap();
    }
    script(
        root.path(),
        &format!(
            "case \"$1\" in inspect) printf '%s' '{}' ;; container) exit 0 ;; *) exit 125 ;; esac",
            inspection("running", true)
        ),
    )
    .unwrap();
    run(root.path(), "protected").unwrap();
}
