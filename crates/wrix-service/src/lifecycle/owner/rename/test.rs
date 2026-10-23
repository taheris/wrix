use super::*;
use std::{num::NonZeroU16, os::unix::fs::PermissionsExt};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn owner() -> Selected {
    Selected {
        runtime: RuntimeKind::Podman,
        workspace: None,
        container: ContainerName::from_persisted("old".into()).unwrap(),
        port: NonZeroU16::new(21232).unwrap(),
        recipe: None,
    }
}
fn journal(native: bool) -> Journal {
    let previous = owner();
    Journal {
        target: Selected {
            container: ContainerName::from_persisted("new".into()).unwrap(),
            ..previous.clone()
        },
        previous,
        native: native.then(|| NativeId::try_from("a".repeat(64)).unwrap()),
    }
}
fn runtime(root: &Path) -> Runtime {
    let script = root.join("podman");
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
root='{root}'
case "$1" in
ps) for name in old new; do [[ ! -f "$root/$name" ]] || printf '%s\n' "$name"; done ;;
port) ;;
rename) [[ "$2" == -- ]]; mv -- "$root/$3" "$root/$4" ;;
inspect)
  [[ -f "$root/$4" ]] || {{ echo 'no such container' >&2; exit 1; }}
  case "$3" in
    '{{{{.Id}}}}') < "$root/$4" head -c 64 ;;
    '{{{{.State.Running}}}}') [[ ! -f "$root/running" ]] && echo false || echo true ;;
    *wrix.service.scope*) echo host ;;
    *wrix.kind*) echo service ;;
    *wrix.cache.enabled*) echo true ;;
    *wrix.cache.backend*) echo celler-v2 ;;
    *wrix.workspace*) echo '<no value>' ;;
    *) echo "unexpected inspect template: $3" >&2; exit 71 ;;
  esac ;;
*) exit 72 ;;
esac
"#,
            root = root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    Runtime {
        binary: script.to_string_lossy().into_owned(),
        kind: RuntimeKind::Podman,
        binding: None,
    }
}

#[test]
fn missing_owner_rename_retains_metadata_and_fences_interrupted_lifecycle() -> TestResult {
    let directory = tempfile::tempdir()?;
    let guard = coordination::acquire(&directory.path().join("host-lifecycle"))?;
    let journal = journal(false);
    journal.previous.record(&guard)?;
    fs::write(
        directory.path().join("retained-data"),
        b"history and dirty state",
    )?;
    journal.persist(guard.root())?;
    assert!(matches!(
        Selected::read(guard.root()),
        Err(super::super::Error::RenamePending)
    ));
    let resumed = Journal::read(&guard.root().join(DIRECTORY))?;
    assert_eq!(resumed.previous, journal.previous);
    assert_eq!(resumed.target, journal.target);
    finish(&runtime(directory.path()), &guard, &resumed)?;
    assert_eq!(Selected::read(guard.root())?, Some(journal.target));
    assert_eq!(
        fs::read(directory.path().join("retained-data"))?,
        b"history and dirty state"
    );
    assert!(!pending(guard.root())?);
    Ok(())
}

#[test]
fn stopped_native_rename_preserves_container_identity_and_retries_after_runtime_commit()
-> TestResult {
    for interrupted in [false, true] {
        let directory = tempfile::tempdir()?;
        let guard = coordination::acquire(&directory.path().join("host-lifecycle"))?;
        let journal = journal(true);
        journal.previous.record(&guard)?;
        journal.persist(guard.root())?;
        fs::write(
            directory
                .path()
                .join(if interrupted { "new" } else { "old" }),
            "a".repeat(64),
        )?;
        finish(&runtime(directory.path()), &guard, &journal)?;
        assert_eq!(Selected::read(guard.root())?, Some(journal.target));
        assert_eq!(
            fs::read_to_string(directory.path().join("new"))?,
            "a".repeat(64)
        );
        assert!(!directory.path().join("old").exists());
    }
    Ok(())
}

#[test]
fn rename_never_adopts_a_different_container_or_changes_running_owners() -> TestResult {
    for (name, id, running) in [("new", 'b', false), ("old", 'b', false), ("old", 'a', true)] {
        let directory = tempfile::tempdir()?;
        let guard = coordination::acquire(&directory.path().join("host-lifecycle"))?;
        let journal = journal(true);
        journal.previous.record(&guard)?;
        journal.persist(guard.root())?;
        fs::write(directory.path().join(name), id.to_string().repeat(64))?;
        if running {
            fs::write(directory.path().join("running"), b"running")?;
        }
        assert!(finish(&runtime(directory.path()), &guard, &journal).is_err());
        assert_eq!(
            Selected::read_during_rename(guard.root())?,
            Some(journal.previous)
        );
        assert!(pending(guard.root())?);
        assert_eq!(
            fs::read_to_string(directory.path().join(name))?,
            id.to_string().repeat(64)
        );
    }
    Ok(())
}

#[test]
fn completed_metadata_write_is_idempotent_and_journal_identity_is_closed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let guard = coordination::acquire(&directory.path().join("host-lifecycle"))?;
    let journal = journal(false);
    journal.previous.record(&guard)?;
    journal.persist(guard.root())?;
    journal.target.persist(guard.root())?;
    finish(&runtime(directory.path()), &guard, &journal)?;
    assert_eq!(Selected::read(guard.root())?, Some(journal.target));
    for id in ["", "short", "../escape", &"z".repeat(64)] {
        assert!(NativeId::try_from(id.to_owned()).is_err());
    }
    Ok(())
}
