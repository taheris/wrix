use super::*;
use std::{path::Path, process::Command};

#[test]
fn krun_sessions_require_an_immutable_native_runtime_not_a_podman_grant_or_alias() {
    let path =
        Krun::parse("/nix/store/00000000000000000000000000000000-runtime/bin/krun".into()).unwrap();
    assert!(Runtime::Krun.matches(Some(&path)));
    assert!(Runtime::Podman.matches(None));
    assert!(!Runtime::Podman.matches(Some(&path)));
    assert!(!Runtime::Krun.matches(None));
    for value in [
        "krun",
        "/usr/bin/krun",
        "/nix/store/runtime/bin/crun",
        "/nix/store/runtime/../bin/krun",
        "/nix/store/00000000000000000000000000000000-/bin/krun",
        "/nix/store/00000000000000000000000000000000-runtime/extra/bin/krun",
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-runtime/bin/krun",
        "/nix/store/00000000000000000000000000000000-runtime/bin/krun\n",
    ] {
        assert!(Krun::parse(value.into()).is_err());
    }
}

#[test]
fn runtime_binding_ignores_agent_arguments_but_rejects_missing_duplicate_or_changed_selection() {
    let path = Path::new("/nix/store/00000000000000000000000000000000-runtime/bin/krun");
    let image = format!("sha256:{}", "a".repeat(64));
    let mut command = Command::new("podman");
    command
        .args(["run", "--rm", "--runtime"])
        .arg(path)
        .arg(&image)
        .args(["--runtime", "agent-argument"]);
    assert!(check_runtime(&command, &image, Some(path)).is_ok());
    assert!(check_runtime(&command, &image, None).is_err());
    for args in [
        vec!["--runtime", "krun"],
        vec!["--runtime", "/another/krun"],
        vec!["--runtime=krun"],
        vec!["--runtime="],
        vec![
            "--runtime",
            path.to_str().unwrap(),
            "--runtime",
            path.to_str().unwrap(),
        ],
    ] {
        let mut command = Command::new("podman");
        command.args(["run", "--rm"]).args(args).arg(&image);
        assert!(check_runtime(&command, &image, Some(path)).is_err());
        assert!(check_runtime(&command, &image, None).is_err());
    }
    let mut command = Command::new("podman");
    command.args(["run", "--rm"]).arg(&image);
    assert!(check_runtime(&command, &image, None).is_ok());
    assert!(check_runtime(&command, &image, Some(path)).is_err());
    assert!(check_runtime(&Command::new("podman"), &image, None).is_err());
}

#[test]
fn option_operands_cannot_masquerade_as_runtime_selection_or_the_image_boundary() {
    let path = Path::new("/nix/store/00000000000000000000000000000000-runtime/bin/krun");
    let image = format!("sha256:{}", "a".repeat(64));
    for option in [
        "-e",
        "--env",
        "-v",
        "--volume",
        "--mount",
        "-w",
        "--workdir",
        "--entrypoint",
        "--label",
        "--name",
    ] {
        let mut command = Command::new("podman");
        command
            .args(["run", "--rm", option])
            .arg(format!("--runtime={}", path.display()))
            .arg(&image);
        assert!(check_runtime(&command, &image, Some(path)).is_err());
        assert!(check_runtime(&command, &image, None).is_ok());
        let mut command = Command::new("podman");
        command
            .args(["run", "--rm", option])
            .arg(&image)
            .args(["--runtime", "other"])
            .arg(&image);
        assert!(check_runtime(&command, &image, None).is_err());
    }
    let mut command = Command::new("podman");
    command
        .args(["run", "--rm", "--unknown", "--runtime"])
        .arg(path)
        .arg(&image);
    assert!(check_runtime(&command, &image, Some(path)).is_err());
}
