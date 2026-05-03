//! Claude Code backend: spawn + shutdown watchdog.
//!
//! [`ClaudeBackend::spawn`] writes the re-pin files into the workspace
//! runtime dir, serializes the [`SpawnConfig`] to JSON, and execs `wrapix
//! run-bead --spawn-config <file> --stdio` with stdin/stdout piped. The
//! watchdog ([`ClaudeBackend::shutdown_after_result`]) handles the
//! post-`result` cleanup: drop the writer, wait `grace`, escalate
//! SIGTERM → SIGKILL.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use loom_core::agent::{
    AgentBackend, AgentSession, Idle, NdjsonReader, ProtocolError, SpawnConfig,
};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::BufWriter;
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use super::parser::ClaudeParser;

/// Subdirectory under the workspace where loom writes claude's runtime
/// files (`repin.sh`, `claude-settings.json`, `spawn-config.json`). The
/// wrapper bind-mounts the workspace into the container at `/workspace`,
/// so the same relative path is visible inside the container — the
/// `SessionStart` hook reads its inputs from `/workspace/.wrapix/loom/runtime`.
const RUNTIME_SUBDIR: &str = ".wrapix/loom/runtime";

/// File name for the JSON-serialized [`SpawnConfig`] handed to
/// `wrapix run-bead --spawn-config`.
const SPAWN_CONFIG_FILE: &str = "spawn-config.json";

/// Default seconds to wait for claude to exit naturally after observing
/// `result`. Per spec the value is configurable via
/// `[agent.claude] post_result_grace_secs`; this constant is the fallback
/// the dispatcher uses when no override is wired up yet.
pub const DEFAULT_POST_RESULT_GRACE_SECS: u64 = 5;

/// Env var that overrides the launcher binary. Production resolves
/// `wrapix` from `PATH`; per-phase config wiring lands in wx-pkht8.9.
const ENV_WRAPIX_BIN: &str = "LOOM_WRAPIX_BIN";

/// Zero-sized marker for the Claude Code stream-json backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`]. The body launches `claude --print
/// --input-format stream-json --output-format stream-json` (via
/// `wrapix run-bead --stdio`) with `--permission-prompt-tool stdio` so
/// tool permissions flow over the same pipe.
pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    async fn spawn(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        let spawn_config_path = prepare_runtime(config)?;

        let wrapix_bin =
            std::env::var_os(ENV_WRAPIX_BIN).unwrap_or_else(|| OsString::from("wrapix"));
        info!(
            wrapix = %wrapix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            "claude backend spawn",
        );

        let mut cmd = Command::new(&wrapix_bin);
        cmd.arg("run-bead")
            .arg("--spawn-config")
            .arg(&spawn_config_path)
            .arg("--stdio");

        spawn_session(cmd, Vec::new()).await
    }
}

impl ClaudeBackend {
    /// Runtime directory used for the re-pin files and serialized
    /// [`SpawnConfig`]. Resolves to a workspace-relative path that the
    /// wrapper bind-mounts into the container.
    pub fn runtime_dir(workspace: &Path) -> PathBuf {
        workspace.join(RUNTIME_SUBDIR)
    }

    /// Run the post-`result` shutdown watchdog: drop the stdin writer (so
    /// claude sees EOF), wait up to `grace` for the child to exit on its
    /// own, then escalate SIGTERM, then SIGKILL.
    ///
    /// Returns the child's exit code (0 if the process was killed via
    /// signal). Errors only when the final `wait` fails — signal-send
    /// failures are logged and treated as best-effort because the child
    /// may already have exited between the wait timeout and the kill.
    pub async fn shutdown_after_result<S>(
        session: AgentSession<S>,
        grace: Duration,
    ) -> Result<i32, ProtocolError> {
        let (mut child, stdin) = session.into_parts();
        drop(stdin);

        if let Some(code) = wait_with_timeout(&mut child, grace).await? {
            return Ok(code);
        }

        warn!(
            grace_ms = grace.as_millis(),
            "claude did not exit after result; sending SIGTERM",
        );
        send_signal(&child, Signal::SIGTERM);

        if let Some(code) = wait_with_timeout(&mut child, grace).await? {
            return Ok(code);
        }

        warn!("claude ignored SIGTERM; sending SIGKILL");
        send_signal(&child, Signal::SIGKILL);

        let status = child.wait().await.map_err(ProtocolError::Io)?;
        Ok(status.code().unwrap_or(0))
    }
}

/// Write the re-pin files and serialize the [`SpawnConfig`] into the
/// workspace runtime dir. Returns the path of the written spawn-config.
///
/// Module-public so tests can verify the side effects independently of
/// the launcher exec (which would otherwise require the real `wrapix`
/// wrapper on `PATH`).
pub(crate) fn prepare_runtime(config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    let runtime_dir = ClaudeBackend::runtime_dir(&config.workspace);
    config
        .repin
        .write_claude_files(&runtime_dir)
        .map_err(ProtocolError::Io)?;
    write_spawn_config(&runtime_dir, config)
}

/// Build an [`AgentSession`] from a launcher [`Command`].
///
/// Module-private — the public surface is [`ClaudeBackend::spawn`]. Tests
/// call this through the `pub(crate)` re-export to substitute a mock claude
/// binary in place of the real `wrapix run-bead` exec.
pub(crate) async fn spawn_session(
    mut cmd: Command,
    denied_tools: Vec<String>,
) -> Result<AgentSession<Idle>, ProtocolError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ProtocolError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("claude child stdin not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("claude child stdout not piped")))?;

    let parser = ClaudeParser::new(denied_tools);
    Ok(AgentSession::new(
        child,
        BufWriter::new(stdin),
        NdjsonReader::new(stdout),
        Box::new(parser),
    ))
}

fn write_spawn_config(runtime_dir: &Path, config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    std::fs::create_dir_all(runtime_dir).map_err(ProtocolError::Io)?;
    let path = runtime_dir.join(SPAWN_CONFIG_FILE);
    let json = serde_json::to_vec(config)?;
    std::fs::write(&path, json).map_err(ProtocolError::Io)?;
    Ok(path)
}

/// Wait `grace` for the child to exit. `Ok(Some(code))` means the child
/// is reaped; `Ok(None)` means the wait timed out and the caller should
/// escalate.
async fn wait_with_timeout(
    child: &mut Child,
    grace: Duration,
) -> Result<Option<i32>, ProtocolError> {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => Ok(Some(status.code().unwrap_or(0))),
        Ok(Err(e)) => Err(ProtocolError::Io(e)),
        Err(_) => Ok(None),
    }
}

/// Best-effort signal send. The child may already be dead (race between
/// the timeout firing and the OS reaping the process); failures are
/// logged but do not propagate so the watchdog can continue its
/// escalation.
fn send_signal(child: &Child, sig: Signal) {
    let Some(pid) = child.id() else {
        debug!("claude child id unavailable; skipping signal {}", sig);
        return;
    };
    let pid = match i32::try_from(pid) {
        Ok(p) => Pid::from_raw(p),
        Err(_) => {
            warn!(pid, "claude child id does not fit in i32; skipping signal");
            return;
        }
    };
    if let Err(e) = kill(pid, sig) {
        debug!(error = %e, signal = %sig, "kill returned error; child may already have exited");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loom_core::agent::{AgentEvent, RePinContent};
    use std::path::PathBuf;
    use std::time::Instant;

    fn sample_repin() -> RePinContent {
        RePinContent {
            orientation: "test orientation".to_string(),
            pinned_context: "test context".to_string(),
            partial_bodies: vec!["partial one".to_string()],
        }
    }

    fn mock_claude_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/loom/mock-claude/claude.sh")
    }

    fn mock_command(mode: &str) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(mock_claude_path()).arg(mode);
        cmd
    }

    // -- test_claude_repin_files -------------------------------------------

    #[test]
    fn prepare_runtime_writes_repin_files_and_spawn_config() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let cfg = SpawnConfig {
            image: "localhost/wrapix-test:claude".to_string(),
            workspace: workspace.path().to_path_buf(),
            env: vec![("WRAPIX_AGENT".into(), "claude".into())],
            initial_prompt: "hello".to_string(),
            agent_args: vec!["--print".into()],
            repin: sample_repin(),
        };

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        let runtime_dir = ClaudeBackend::runtime_dir(workspace.path());
        assert!(
            runtime_dir.join("repin.sh").exists(),
            "repin.sh missing under {}",
            runtime_dir.display(),
        );
        assert!(
            runtime_dir.join("claude-settings.json").exists(),
            "claude-settings.json missing under {}",
            runtime_dir.display(),
        );
        assert_eq!(spawn_config_path, runtime_dir.join(SPAWN_CONFIG_FILE));
        assert!(spawn_config_path.exists());

        // Round-trip the spawn-config file to confirm it carries the input
        // unchanged — the wrapper consumes this exact JSON.
        let bytes = std::fs::read(&spawn_config_path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.image, cfg.image);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        assert_eq!(decoded.agent_args, cfg.agent_args);
    }

    // -- test_claude_supports_steering -------------------------------------

    #[tokio::test]
    async fn steering_message_reaches_mock_and_emits_followup_turn() {
        let session = spawn_session(mock_command("steering"), Vec::new())
            .await
            .expect("spawn session");
        let mut session = session.prompt("first prompt").await.expect("prompt ok");

        // First assistant turn — proves the mock saw the prompt.
        match session.next_event().await.expect("event ok") {
            Some(AgentEvent::MessageDelta { text }) => {
                assert!(text.contains("first turn"), "unexpected text: {text}");
            }
            other => panic!("expected first MessageDelta, got {other:?}"),
        }

        session
            .steer("STEERED_TEXT")
            .await
            .expect("steer should succeed");

        // Second assistant turn — proves steering reached the mock.
        match session.next_event().await.expect("event ok") {
            Some(AgentEvent::MessageDelta { text }) => {
                assert!(
                    text.contains("STEERED_TEXT"),
                    "second turn did not echo steer: {text}",
                );
            }
            other => panic!("expected second MessageDelta, got {other:?}"),
        }

        // Drain remaining events until SessionComplete so the watchdog has
        // a result to consume.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }

        let exit = ClaudeBackend::shutdown_after_result(session, Duration::from_millis(500))
            .await
            .expect("shutdown ok");
        assert_eq!(exit, 0, "mock exited cleanly after result");
    }

    // -- test_claude_shutdown_watchdog -------------------------------------

    #[tokio::test]
    async fn shutdown_watchdog_escalates_to_sigkill_when_child_ignores_stdin_close() {
        let session = spawn_session(mock_command("ignore-stdin"), Vec::new())
            .await
            .expect("spawn session");
        let mut session = session.prompt("hello").await.expect("prompt ok");

        // Drive events until SessionComplete arrives.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }

        let grace = Duration::from_millis(150);
        let started = Instant::now();
        let _exit = ClaudeBackend::shutdown_after_result(session, grace)
            .await
            .expect("shutdown ok");
        let elapsed = started.elapsed();

        // The mock traps SIGTERM but cannot trap SIGKILL. The watchdog
        // must walk the full SIGTERM → SIGKILL escalation, so the total
        // elapsed time exceeds two grace windows but stays well under any
        // reasonable test budget.
        assert!(
            elapsed >= grace * 2,
            "watchdog returned too early: {elapsed:?} < 2 × {grace:?}",
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "watchdog took longer than expected: {elapsed:?}",
        );
    }
}
