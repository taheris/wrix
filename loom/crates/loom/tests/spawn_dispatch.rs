//! Cross-cutting integration test: host -> wrapix spawn -> agent dispatch.
//!
//! Verifies the contract loom owes the wrapix wrapper:
//!
//! 1. `wrapix spawn --spawn-config <file> --stdio` is the only argv shape
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

/// Resolve the absolute path to `bash` from `PATH`. Used so the shim's
/// shebang points at a concrete interpreter rather than `/usr/bin/env`,
/// which is not present in the default nix-build sandbox (`sandbox = true`).
fn find_bash() -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("bash");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("bash not found in PATH");
}

/// Write the wrapix shim into `dir` and return its path. The shim records
/// argv (one quoted token per line) and stdin TTY/pipe state into the two
/// sibling files, copies the `--spawn-config` JSON aside (so the test can
/// inspect it without racing the temp-file delete), then exec's mock-pi in
/// `happy-path` mode so the pi backend handshake AND the prompt round-trip
/// complete; otherwise the loom binary would hang waiting for `agent_end`.
fn install_wrapix_shim(
    dir: &Path,
    argv_file: &Path,
    stdin_info: &Path,
    spawn_config_copy: &Path,
    mock_pi: &Path,
) -> PathBuf {
    let shim = dir.join("wrapix");
    let bash = find_bash();
    let body = format!(
        "#!{bash}\n\
         set -euo pipefail\n\
         ARGV_FILE='{argv}'\n\
         STDIN_INFO='{stdin}'\n\
         SPAWN_CONFIG_COPY='{copy}'\n\
         MOCK_PI='{mock}'\n\
         \n\
         # Record argv (one token per line) so the test can pin the exact\n\
         # invocation shape — `spawn --spawn-config <file> --stdio`.\n\
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
         exec '{bash}' \"$MOCK_PI\" happy-path\n",
        bash = bash.display(),
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
    // Spawn-bound subcommands (`todo` is one) read LOOM_PROFILES_MANIFEST at
    // startup (wx-3hhwq.32). The production todo controller now resolves the
    // `base` profile through this manifest (wx-wlwv3), so it must contain a
    // real entry — an empty `{}` would surface as ProfileError::UnknownProfile.
    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").expect("write stub image source");
    let manifest_body = format!(
        r#"{{
          "base": {{ "ref": "localhost/wrapix-base:test", "source": {source:?} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).expect("write manifest stub");
    init_workspace_repo(workspace);
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
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .output()
        .expect("spawn loom")
}

/// Seed the workspace as a real git repo. `loom todo` opens a `GitClient`
/// during setup (wx-9z0nq) so the tier-1 detection has a real ref database
/// to query — even when the test exits before any tier-1 work happens.
fn init_workspace_repo(workspace: &Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .expect("git spawn");
        assert!(status.success(), "git {args:?} failed: {status}");
    }
    std::fs::write(workspace.join(".gitignore"), "shim/\n*.tar\n").expect("write .gitignore");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["commit", "-q", "--allow-empty", "-m", "seed"])
        .status()
        .expect("git commit spawn");
    assert!(status.success(), "git commit failed: {status}");
}

/// `tests/loom-test.sh::test_wrapix_spawn_dispatch` — loom hands the
/// wrapper exactly `wrapix spawn --spawn-config <file> --stdio`, and
/// the file resolves to a JSON [`SpawnConfig`] carrying the per-bead
/// profile image. A future profile-resolution change that drops the
/// `image_ref`/`image_source` fields or renames the subcommand will trip
/// this assertion before the wrapper ever sees the malformed argv.
#[test]
fn wrapix_spawn_invocation_records_correct_argv() {
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
        Some("spawn"),
        "first arg must be spawn. argv={tokens:?}",
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
    // the resolved image_ref + image_source from the manifest written by
    // `drive_loom_todo_pi` (`base` profile maps to `localhost/wrapix-base:test`).
    let bytes = std::fs::read(&spawn_copy).expect("shim should copy spawn-config aside");
    let cfg: loom_core::agent::SpawnConfig =
        serde_json::from_slice(&bytes).expect("spawn-config must deserialize");
    assert_eq!(
        cfg.image_ref, "localhost/wrapix-base:test",
        "spawn-config image_ref must match the resolved profile image",
    );
    assert!(
        !cfg.image_source.as_os_str().is_empty(),
        "spawn-config image_source must be populated. got={}",
        cfg.image_source.display(),
    );
    assert!(
        cfg.initial_prompt.contains("loom-agent"),
        "initial prompt should reference the spec label. prompt={}",
        cfg.initial_prompt,
    );
}

/// `tests/loom-test.sh::test_loom_todo_writes_ndjson_log` — the run-time
/// promise from `specs/loom-harness.md` *Run UX & Logging* is that every
/// `loom todo` invocation emits a per-phase NDJSON file under
/// `<workspace>/.wrapix/loom/logs/<spec-label>/todo-<utc>.ndjson`. Without
/// this gate the workflow happily ran agents to completion while
/// `run_agent` discarded every event with a `trace!` call (wx-2emcs);
/// users saw two INFO lines and an empty `loom logs`. The test drives the
/// same mock-pi handshake as the dispatch tests above, then asserts the
/// log file appears at the documented path with at least one valid event
/// line that round-trips through `serde_json`. A future regression that
/// removes the sink wiring trips this assertion before any user-visible
/// breakage.
#[test]
fn loom_todo_writes_ndjson_log_under_workspace_logs_dir() {
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

    // The phase log path is `<workspace>/.wrapix/loom/logs/<spec>/todo-<utc>.ndjson`.
    // Spec label was passed as `loom-agent` via `drive_loom_todo_pi`.
    let logs_dir = workspace.join(".wrapix/loom/logs/loom-agent");
    assert!(
        logs_dir.is_dir(),
        "phase log directory must exist after `loom todo`: {}\nstdout={stdout}\nstderr={stderr}",
        logs_dir.display(),
    );
    let entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("read logs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ndjson"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one NDJSON file must appear under {}: got {entries:?}",
        logs_dir.display(),
    );
    let log_path = &entries[0];
    let stem = log_path.file_stem().and_then(|s| s.to_str()).unwrap();
    assert!(
        stem.starts_with("todo-"),
        "phase log file stem must start with `todo-`: got {stem}",
    );

    let body = std::fs::read_to_string(log_path).expect("read log");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "log file must contain at least one event line, got empty body. path={}",
        log_path.display(),
    );
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}\nline={line}"));
        assert!(
            v.get("kind").and_then(|k| k.as_str()).is_some(),
            "every event must carry a `kind` field. line {i}: {line}",
        );
    }
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["kind"], "session_complete",
        "the final event must be session_complete. lines={lines:?}",
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
