use std::{fs, os::unix::fs::PermissionsExt, process::Command};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn unsupported_arguments_fail_before_any_subprocess() -> TestResult {
    let fixture = tempfile::tempdir()?;
    let bin = fixture.path().join("bin");
    let calls = fixture.path().join("calls");
    fs::create_dir(&bin)?;
    for name in ["git", "bd", "podman", "container", "nix", "dolt"] {
        let path = bin.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{name}' >>'{}'\nexit 91\n",
                calls.display()
            ),
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let mut commands = vec![vec!["beads", "push"]];
    for command in ["start", "stop", "status", "logs", "endpoints"] {
        commands.push(vec!["service", command]);
    }
    for command in [
        "status",
        "socket",
        "port",
        "host",
        "sandbox-endpoint",
        "attach",
        "gc",
        "wait",
    ] {
        commands.push(vec!["service", "dolt", command]);
    }
    for command in ["status", "publish", "warm", "prune", "rotate-key"] {
        commands.push(vec!["service", "cache", command]);
    }
    for args in commands {
        for invalid in ["--dry-run", "surplus"] {
            let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
                .args(&args)
                .arg(invalid)
                .current_dir(fixture.path())
                .env("PATH", &bin)
                .env("HOME", fixture.path().join("home"))
                .output()?;
            let stderr = String::from_utf8(output.stderr)?;
            assert!(!output.status.success(), "{args:?} {invalid}: {stderr}");
            assert!(stderr.contains(invalid), "{args:?}: {stderr}");
            assert!(
                !calls.exists(),
                "{args:?} {invalid} invoked a subprocess: {stderr}"
            );
        }
    }
    Ok(())
}
