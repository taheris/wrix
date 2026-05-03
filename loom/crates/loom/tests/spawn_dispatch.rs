//! Cross-cutting integration test: host -> wrapix run-bead -> agent dispatch.
//!
//! Verifies the contract loom owes the wrapix wrapper:
//!
//! 1. `wrapix run-bead --spawn-config <file> --stdio` is the only argv shape
//!    loom hands to the wrapper. `<file>` resolves to a JSON-serialized
//!    [`SpawnConfig`] containing the resolved profile image.
//! 2. The container child receives stdin via a pipe (not a TTY) so NDJSON
//!    framing flows correctly and EOF semantics work when loom closes its
//!    end of the pipe.
//!
//! Both tests drive `loom --agent pi todo` through a wrapix shim that
//! records what the loom binary actually exec'd. The shim then hands the
//! exchange off to the existing `mock-pi.sh` so the pi backend's startup
//! probe + prompt round-trip completes naturally — without that, the loom
//! binary would hang waiting for `agent_end` and the test would never see
//! the recorded argv.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Write the wrapix shim into `dir` and return its path. The shim records
/// argv (one quoted token per line) and stdin TTY/pipe state into the two
/// sibling files, copies the `--spawn-config` JSON aside (so the test can
/// inspect it without racing the temp-file delete), then exec's mock-pi in
/// `probe-ok` mode to satisfy the pi backend handshake.
fn install_wrapix_shim(
    dir: &Path,
    argv_file: &Path,
    stdin_info: &Path,
    spawn_config_copy: &Path,
    mock_pi: &Path,
) -> PathBuf {
    let shim = dir.join("wrapix");
    let body = format!(
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         ARGV_FILE='{argv}'\n\
         STDIN_INFO='{stdin}'\n\
         SPAWN_CONFIG_COPY='{copy}'\n\
         MOCK_PI='{mock}'\n\
         \n\
         # Record argv (one token per line) so the test can pin the exact\n\
         # invocation shape — `run-bead --spawn-config <file> --stdio`.\n\
         {{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > \"$ARGV_FILE\"\n\
         \n\
         # Record stdin properties so the pipe-not-tty contract is\n\
         # observable from inside the container. -t 0 returns 0 only when\n\
         # fd 0 is a real terminal; -p /dev/stdin returns 0 only when fd 0\n\
         # is a FIFO/pipe — together they distinguish pipe vs tty vs file.\n\
         {{ if [ -t 0 ]; then echo 'stdin_is_tty=1'; else echo 'stdin_is_tty=0'; fi\n\
            if [ -p /dev/stdin ]; then echo 'stdin_is_pipe=1'; else echo 'stdin_is_pipe=0'; fi\n\
         }} > \"$STDIN_INFO\"\n\
         \n\
         # Pull the --spawn-config <path> out of argv and stash a copy so\n\
         # the test can verify the JSON shape. PiBackend writes the file\n\
         # under /tmp; the original path is fine to read but copying makes\n\
         # the test independent of the loom binary's cleanup behavior.\n\
         prev=''\n\
         for a in \"$@\"; do\n\
             if [ \"$prev\" = '--spawn-config' ]; then\n\
                 cp \"$a\" \"$SPAWN_CONFIG_COPY\"\n\
                 break\n\
             fi\n\
             prev=\"$a\"\n\
         done\n\
         \n\
         # Hand stdin/stdout to mock-pi for the protocol exchange. exec\n\
         # replaces this shell so the kernel-level pipe routing matches the\n\
         # production case where wrapix execs podman which execs the agent.\n\
         exec bash \"$MOCK_PI\" probe-ok\n",
        argv = argv_file.display(),
        stdin = stdin_info.display(),
        copy = spawn_config_copy.display(),
        mock = mock_pi.display(),
    );
    std::fs::write(&shim, body).unwrap();
    let mut perm = std::fs::metadata(&shim).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).unwrap();
    shim
}

/// Locate `tests/loom/mock-pi/pi.sh` relative to the loom-binary crate.
/// Mirrors the helper used by the loom-agent unit tests so the same mock
/// drives the integration harness.
fn mock_pi_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/loom/mock-pi/pi.sh")
}

/// Run `loom --workspace <ws> --agent pi todo -s loom-agent` against a
/// shim wrapix and return the captured `Output`. Shared by both tests so
/// the assertions stay focused on what they verify.
fn drive_loom_todo_pi(workspace: &Path, shim: &Path, loom_bin: &str) -> std::process::Output {
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("todo")
        .arg("-s")
        .arg("loom-agent")
        .env("LOOM_WRAPIX_BIN", shim)
        .env("LOOM_BIN", loom_bin)
        .output()
        .expect("spawn loom")
}

/// `tests/loom-test.sh::test_wrapix_run_bead_spawn` — loom hands the
/// wrapper exactly `wrapix run-bead --spawn-config <file> --stdio`, and
/// the file resolves to a JSON [`SpawnConfig`] carrying the per-bead
/// profile image. A future profile-resolution change that drops `image`
/// or renames the subcommand will trip this assertion before the wrapper
/// ever sees the malformed argv.
#[test]
fn wrapix_run_bead_invocation_records_correct_argv() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrapix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );

    let argv = std::fs::read_to_string(&argv_file).expect("shim should record argv");
    let tokens: Vec<&str> = argv.lines().collect();
    assert_eq!(
        tokens.first().copied(),
        Some("run-bead"),
        "first arg must be run-bead. argv={tokens:?}",
    );
    let spawn_idx = tokens
        .iter()
        .position(|t| *t == "--spawn-config")
        .unwrap_or_else(|| panic!("--spawn-config flag missing from argv. argv={tokens:?}"));
    let spawn_config_path = tokens.get(spawn_idx + 1).unwrap_or_else(|| {
        panic!("--spawn-config without a value. argv={tokens:?}");
    });
    assert!(
        Path::new(spawn_config_path).is_absolute(),
        "spawn-config path must be absolute (wrapper consumes it from /tmp). got={spawn_config_path}",
    );
    assert!(
        tokens.contains(&"--stdio"),
        "--stdio flag missing from argv. argv={tokens:?}",
    );

    // The spawn-config JSON must round-trip through SpawnConfig and carry
    // the production controller's image. ProductionTodoController hard-codes
    // `wrapix-base:latest` until per-bead profile resolution lands; this
    // assertion will need updating then, but pinning the current contract
    // catches accidental drops of the `image` field.
    let bytes = std::fs::read(&spawn_copy).expect("shim should copy spawn-config aside");
    let cfg: loom_core::agent::SpawnConfig =
        serde_json::from_slice(&bytes).expect("spawn-config must deserialize");
    assert_eq!(
        cfg.image, "wrapix-base:latest",
        "spawn-config image must match the resolved profile image",
    );
    assert!(
        cfg.initial_prompt.contains("loom-agent"),
        "initial prompt should reference the spec label. prompt={}",
        cfg.initial_prompt,
    );
}

/// `tests/loom-test.sh::test_container_stdio_pipe` — the agent process
/// receives stdin as a pipe, never a TTY. EOF on that pipe is the signal
/// loom uses to tell the agent "no more input is coming"; if the
/// underlying handle were a TTY (or a regular file), the agent's `read`
/// would either block or return non-EOF, breaking the shutdown contract.
#[test]
fn child_stdin_is_a_pipe_not_a_tty() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrapix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );

    let info = std::fs::read_to_string(&stdin_info).expect("shim should record stdin info");
    assert!(
        info.contains("stdin_is_tty=0"),
        "child stdin must NOT be a TTY (got {info:?})",
    );
    assert!(
        info.contains("stdin_is_pipe=1"),
        "child stdin must be a pipe — both backends call Stdio::piped() (got {info:?})",
    );

    // The mock-pi handshake completing end-to-end is the second half of
    // the EOF contract: the pi backend writes get_commands then prompt
    // through the same pipe, mock-pi reads each line, responds, and the
    // session reaches agent_end. If stdin were not a pipe, those `read`
    // calls would either block forever (TTY without echo) or return
    // wrong data; either way `loom todo` would not exit 0.
    assert!(
        stdout.contains("loom todo:"),
        "expected the loom todo summary line, indicating the agent reached agent_end. \
         stdout={stdout} stderr={stderr}",
    );
}
