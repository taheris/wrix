mod common;

use std::{
    collections::HashMap, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command,
};

use common::{TestResult, run_command, set_mode};
use serde::Deserialize;

#[derive(Deserialize)]
struct BuildSpec {
    builder: PathBuf,
    args: Vec<String>,
    env: HashMap<String, String>,
}

#[test]
fn real_bundle_builder_works_without_root_write_privileges() -> TestResult {
    let runtime = common::prek::runtime()?;
    let spec: BuildSpec = serde_json::from_slice(&fs::read(&runtime.builder_spec)?)?;
    // Nix's TMPDIR may have a private parent owned by the root builder.
    // A child running as nobody must be able to traverse every parent.
    let work = tempfile::tempdir_in("/tmp")?;
    set_mode(work.path(), 0o777)?;
    let out = work.path().join("out");
    let mut command = Command::new(spec.builder);
    command
        .args(spec.args)
        .env_clear()
        .envs(spec.env)
        .current_dir(work.path())
        .env("out", &out)
        .env("NIX_BUILD_TOP", work.path())
        .env("TMPDIR", work.path())
        .env("TMP", work.path())
        .env("TEMPDIR", work.path())
        .env("HOME", work.path())
        .env("PATH", "/path-not-set")
        .env("NIX_LOG_FD", "2");
    drop_root_privileges(&mut command)?;
    let result = run_command(&mut command)?;
    assert!(
        result.status.success(),
        "real unprivileged builder failed: {} {}",
        result.stdout,
        result.stderr
    );
    for name in [
        "_binding.sh",
        "pre-commit",
        "pre-push",
        "post-checkout",
        "post-merge",
        "prepare-commit-msg",
    ] {
        let actual = out.join(name);
        let expected = runtime.hooks.join(name);
        assert_eq!(fs::read(&actual)?, fs::read(&expected)?, "{name}");
        assert_eq!(
            fs::metadata(actual)?.permissions().mode() & 0o111,
            fs::metadata(expected)?.permissions().mode() & 0o111,
            "{name} executable mode"
        );
    }
    Ok(())
}

fn drop_root_privileges(command: &mut Command) -> TestResult {
    let result = run_command(Command::new("id").arg("-u"))?;
    if !result.status.success() {
        return Err(format!("cannot determine test UID: {}", result.stderr).into());
    }
    if result.stdout.trim() == "0" {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            command.uid(65534).gid(65534);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = command;
            return Err("run this packaging regression as an unprivileged user".into());
        }
    }
    Ok(())
}
