use std::{fs, path::Path, process::Command};

use super::common::{TestResult, set_mode};

const PODMAN: &str = r#"#!/usr/bin/env bash
set -euo pipefail
root=$(dirname "$0")
printf '%s\n' "$*" >>"$root/peer.log"
name=$(cat "$root/peer.name")
case "$*" in
  'ps -a --format {{.Names}}')
    if [[ $(cat "$root/peer.state") == unavailable ]]; then
      printf 'cannot connect to Podman machine\n' >&2
      exit 125
    fi
    printf '%s\n' "$name"
    ;;
  "port $name") ;;
  *)
    if [[ $# != 4 || $1 != inspect || $2 != --format || $4 != "$name" ]]; then
      printf 'unexpected peer runtime command: %s\n' "$*" >&2
      exit 64
    fi
    case "$3" in
      '{{ index .Config.Labels "wrix.kind" }}') printf 'service\n' ;;
      '{{ index .Config.Labels "wrix.workspace.hash" }}') printf 'peer\n' ;;
      '{{ index .Config.Labels "wrix.workspace" }}') printf '/peer\n' ;;
      '{{ index .Config.Labels "wrix.cache.backend" }}') cat "$root/peer.backend" ;;
      '{{ index .Config.Labels "wrix.cache.enabled" }}') printf 'true\n' ;;
      '{{.State.Running}}') cat "$root/peer.state" ;;
      *) printf 'unexpected peer runtime format: %s\n' "$3" >&2; exit 64 ;;
    esac
    ;;
esac
"#;

pub fn install(bin: &Path, name: &str, backend: &str, state: &str) -> TestResult {
    fs::write(bin.join("peer.name"), name)?;
    fs::write(bin.join("peer.backend"), backend)?;
    fs::write(bin.join("peer.state"), state)?;
    fs::write(bin.join("podman"), PODMAN)?;
    set_mode(&bin.join("podman"), 0o755)?;
    Ok(())
}

#[test]
fn fixture_implements_native_podman_inventory_and_inspection() -> TestResult {
    let directory = tempfile::tempdir()?;
    install(directory.path(), "peer", "celler-v2", "true")?;
    let binary = directory.path().join("podman");
    for (args, expected) in [
        (vec!["ps", "-a", "--format", "{{.Names}}"], "peer"),
        (
            vec!["inspect", "--format", "{{.State.Running}}", "peer"],
            "true",
        ),
        (
            vec![
                "inspect",
                "--format",
                "{{ index .Config.Labels \"wrix.cache.backend\" }}",
                "peer",
            ],
            "celler-v2",
        ),
        (vec!["port", "peer"], ""),
    ] {
        let output = Command::new(&binary).args(args).output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?.trim(), expected);
    }
    assert_eq!(
        Command::new(&binary).arg("rm").output()?.status.code(),
        Some(64)
    );
    fs::write(directory.path().join("peer.state"), "unavailable")?;
    assert_eq!(
        Command::new(&binary)
            .args(["ps", "-a", "--format", "{{.Names}}"])
            .output()?
            .status
            .code(),
        Some(125)
    );
    Ok(())
}
