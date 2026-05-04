//! Pi-mono RPC backend: spawn + startup probe + optional `set_model`.
//!
//! [`PiBackend::spawn`] serializes the [`SpawnConfig`] to a JSON file,
//! execs `wrapix run-bead --spawn-config <file> --stdio` (the wrapper that
//! owns container construction), and drives the pi RPC handshake before
//! handing back an [`AgentSession`] in the [`Idle`] state:
//!
//! 1. `get_commands` probe — verifies pi exposes every command Loom
//!    depends on (`prompt`, `steer`, `abort`, `set_model`). A missing
//!    required command surfaces as [`ProtocolError::Unsupported`] so a
//!    version mismatch is caught before any workflow begins.
//! 2. `set_model` (optional) — sent only when [`SpawnConfig::model`] is
//!    populated by per-phase config. Failure is hard-fail.
//!
//! Process IO during the handshake is direct (no [`AgentSession`] yet) —
//! the typestate session only starts taking events once `prompt` is
//! called by the workflow layer. Compaction re-pin is the workflow
//! layer's responsibility (driven from `AgentEvent::CompactionStart`); the
//! backend itself does not own re-pin policy.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use loom_core::agent::{
    AgentBackend, AgentSession, Idle, ModelSelection, NdjsonReader, ProtocolError, SpawnConfig,
};
use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::{ChildStdin, Command};
use tracing::{debug, error, info};

use super::messages::{PiEnvelope, PiResponse};
use super::parser::PiParser;

/// Env var that overrides the launcher binary. Production resolves
/// `wrapix` from `PATH`; tests substitute the mock pi script via this.
const ENV_WRAPIX_BIN: &str = "LOOM_WRAPIX_BIN";

/// Probe id used for the startup `get_commands` request. The id appears in
/// pi's response so the backend can correlate request/response without
/// blocking on intervening events.
const PROBE_REQUEST_ID: &str = "loom-pi-probe";

/// Request id used for the optional post-probe `set_model` request.
const SET_MODEL_REQUEST_ID: &str = "loom-pi-set-model";

/// Pi commands Loom depends on. A missing entry in the `get_commands`
/// response is a hard fail (`ProtocolError::Unsupported`).
const REQUIRED_COMMANDS: &[&str] = &["prompt", "steer", "abort", "set_model"];

/// Counter that distinguishes simultaneous spawn-config files inside the
/// same loom process. The pid component handles cross-process uniqueness.
static SPAWN_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Zero-sized marker for the pi-mono RPC backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`] — the backend itself carries no fields. The type
/// parameter alone is what dispatches `<B: AgentBackend>` call sites in
/// `loom-workflow` (`run_agent::<PiBackend>(..)` versus
/// `run_agent::<ClaudeBackend>(..)`).
pub struct PiBackend;

impl AgentBackend for PiBackend {
    async fn spawn(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        let spawn_config_path = write_spawn_config(config)?;

        let wrapix_bin =
            std::env::var_os(ENV_WRAPIX_BIN).unwrap_or_else(|| OsString::from("wrapix"));
        info!(
            wrapix = %wrapix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            "pi backend spawn",
        );

        let mut cmd = Command::new(&wrapix_bin);
        cmd.arg("run-bead")
            .arg("--spawn-config")
            .arg(&spawn_config_path)
            .arg("--stdio");

        spawn_with_handshake(cmd, config.model.as_ref()).await
    }
}

/// `get_commands` request body. Sent on stdin during the startup handshake
/// before any [`AgentSession`] is constructed.
#[derive(Serialize)]
struct GetCommandsCommand<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
}

/// `set_model` request body. Sent only when [`SpawnConfig::model`] is
/// populated; the wrapper never sees this — it is consumed by pi inside
/// the container.
#[derive(Serialize)]
struct SetModelCommand<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    provider: &'a str,
    #[serde(rename = "modelId")]
    model_id: &'a str,
}

/// Spawn the launcher [`Command`], drive the startup handshake (probe +
/// optional `set_model`), and return a session in the [`Idle`] state.
///
/// Module-public so unit tests can substitute a mock pi binary in place
/// of the real `wrapix run-bead` exec without going through the
/// `LOOM_WRAPIX_BIN` env-var override.
pub(crate) async fn spawn_with_handshake(
    mut cmd: Command,
    model: Option<&ModelSelection>,
) -> Result<AgentSession<Idle>, ProtocolError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ProtocolError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("pi child stdin not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("pi child stdout not piped")))?;

    let mut writer = BufWriter::new(stdin);
    let mut reader = NdjsonReader::new(stdout);

    run_probe(&mut writer, &mut reader).await?;

    if let Some(model) = model {
        run_set_model(&mut writer, &mut reader, model).await?;
    }

    let parser = PiParser::new();
    Ok(AgentSession::new(child, writer, reader, Box::new(parser)))
}

/// Send `get_commands` on stdin and wait for the matching response. Events
/// emitted before the response are observed but ignored — pi can interleave
/// telemetry around request handling, so the loop drains lines until the
/// correlated response arrives.
async fn run_probe(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut NdjsonReader,
) -> Result<(), ProtocolError> {
    let cmd = GetCommandsCommand {
        kind: "get_commands",
        id: PROBE_REQUEST_ID,
    };
    write_command(writer, &cmd).await?;

    let resp = await_response(reader, PROBE_REQUEST_ID).await?;
    if !resp.success {
        error!(
            error = ?resp.error,
            "pi get_commands probe failed",
        );
        return Err(ProtocolError::Unsupported);
    }

    let commands = extract_commands(&resp)?;
    let missing: Vec<&&str> = REQUIRED_COMMANDS
        .iter()
        .filter(|req| !commands.iter().any(|c| c == *req))
        .collect();
    if !missing.is_empty() {
        error!(
            missing = ?missing,
            available = ?commands,
            "pi get_commands probe missing required commands — version mismatch",
        );
        return Err(ProtocolError::Unsupported);
    }

    debug!(commands = ?commands, "pi get_commands probe succeeded");
    Ok(())
}

/// Send `set_model` on stdin and wait for the matching response. A failure
/// response is a hard fail — Loom requires the requested model to take
/// effect before the workflow begins.
async fn run_set_model(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut NdjsonReader,
    model: &ModelSelection,
) -> Result<(), ProtocolError> {
    let cmd = SetModelCommand {
        kind: "set_model",
        id: SET_MODEL_REQUEST_ID,
        provider: &model.provider,
        model_id: &model.model_id,
    };
    write_command(writer, &cmd).await?;

    let resp = await_response(reader, SET_MODEL_REQUEST_ID).await?;
    if !resp.success {
        error!(
            error = ?resp.error,
            provider = %model.provider,
            model_id = %model.model_id,
            "pi set_model failed",
        );
        return Err(ProtocolError::Unsupported);
    }

    info!(
        provider = %model.provider,
        model_id = %model.model_id,
        "pi set_model succeeded",
    );
    Ok(())
}

/// Encode `payload` as NDJSON and flush it to pi's stdin.
async fn write_command<T: Serialize>(
    writer: &mut BufWriter<ChildStdin>,
    payload: &T,
) -> Result<(), ProtocolError> {
    let mut line = serde_json::to_string(payload)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read NDJSON lines until one classifies as the `response` matching
/// `expected_id`. Other lines (events, unrelated responses, extension UI
/// requests) are observed and dropped — request/response correlation is
/// the only contract this loop enforces.
async fn await_response(
    reader: &mut NdjsonReader,
    expected_id: &str,
) -> Result<PiResponse, ProtocolError> {
    loop {
        let line_owned = match reader.next_line().await? {
            Some(line) => line.to_owned(),
            None => return Err(ProtocolError::UnexpectedEof),
        };
        let env: PiEnvelope = serde_json::from_str(&line_owned)?;
        if env.msg_type.as_deref() == Some("response") {
            let resp: PiResponse = serde_json::from_str(&line_owned)?;
            if resp.id.as_str() == expected_id {
                return Ok(resp);
            }
            debug!(
                got = %resp.id,
                want = %expected_id,
                "pi response id mismatch — discarding",
            );
        } else {
            debug!(
                msg_type = ?env.msg_type,
                "pi handshake observed non-response line — discarding",
            );
        }
    }
}

/// Pull the command list out of a successful `get_commands` response.
/// Pi v0.72+ returns a JSON array of strings under `data`; anything else is
/// a protocol-shape mismatch.
fn extract_commands(resp: &PiResponse) -> Result<Vec<String>, ProtocolError> {
    let data = resp.data.as_ref().ok_or_else(|| {
        error!("pi get_commands response missing `data`");
        ProtocolError::Unsupported
    })?;
    let arr = data.as_array().ok_or_else(|| {
        error!(data = %data, "pi get_commands `data` is not an array");
        ProtocolError::Unsupported
    })?;
    arr.iter()
        .map(|v| {
            v.as_str().map(str::to_owned).ok_or_else(|| {
                error!(entry = %v, "pi get_commands entry is not a string");
                ProtocolError::Unsupported
            })
        })
        .collect()
}

/// Serialize `config` as JSON and write it to a uniquely-named tempfile
/// under the system temp dir. The path is handed to `wrapix run-bead
/// --spawn-config`; the wrapper reads it back and ignores any unknown
/// fields (`model` is consumed by the host-side backend, not the wrapper).
fn write_spawn_config(config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let counter = SPAWN_CONFIG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("loom-{pid}-{counter}.json"));
    write_spawn_config_to(&path, config)?;
    Ok(path)
}

fn write_spawn_config_to(path: &Path, config: &SpawnConfig) -> Result<(), ProtocolError> {
    let json = serde_json::to_vec(config)?;
    std::fs::write(path, json).map_err(ProtocolError::Io)?;
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_core::agent::{AgentEvent, RePinContent};
    use std::path::PathBuf;

    fn mock_pi_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/loom/mock-pi/pi.sh")
    }

    fn mock_command(mode: &str) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(mock_pi_path()).arg(mode);
        cmd
    }

    fn sample_repin() -> RePinContent {
        RePinContent {
            orientation: "loom run @ wx-test".to_string(),
            pinned_context: "Spec: specs/loom-agent.md".to_string(),
            partial_bodies: vec!["partial alpha".to_string()],
        }
    }

    fn sample_config(model: Option<ModelSelection>) -> SpawnConfig {
        SpawnConfig {
            image: "localhost/wrapix-test:pi".to_string(),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRAPIX_AGENT".into(), "pi".into())],
            initial_prompt: "hello pi".to_string(),
            agent_args: vec![],
            repin: sample_repin(),
            model,
        }
    }

    // -- write_spawn_config -----------------------------------------------

    #[test]
    fn write_spawn_config_round_trips_through_json() {
        let cfg = sample_config(Some(ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        }));
        let path = write_spawn_config(&cfg).expect("write");
        let bytes = std::fs::read(&path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.image, cfg.image);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        let model = decoded.model.expect("model present");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.model_id, "deepseek-v3");
        let _ = std::fs::remove_file(&path);
    }

    // -- test_pi_startup_probe --------------------------------------------

    #[tokio::test]
    async fn startup_probe_succeeds_when_required_commands_present() {
        let session = spawn_with_handshake(mock_command("happy-path"), None)
            .await
            .expect("probe should succeed");
        // Drive a prompt to confirm the session is wired and the mock keeps
        // running past the probe.
        let mut session = session.prompt("ping").await.expect("prompt ok");
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
    }

    #[tokio::test]
    async fn startup_probe_fails_fast_when_required_command_missing() {
        let result = spawn_with_handshake(mock_command("probe-missing-set-model"), None).await;
        match result {
            Err(ProtocolError::Unsupported) => {}
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("probe should have failed"),
        }
    }

    // -- test_pi_rpc_command_sending --------------------------------------

    #[tokio::test]
    async fn driver_sends_prompt_as_ndjson_line() {
        let session = spawn_with_handshake(mock_command("echo-prompt"), None)
            .await
            .expect("spawn");
        let mut session = session.prompt("HELLO_PROMPT").await.expect("prompt ok");

        let mut saw_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::MessageDelta { text }) => {
                    if text.contains("HELLO_PROMPT") {
                        saw_echo = true;
                    }
                }
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_echo, "mock did not echo the prompt");
    }

    // -- test_pi_supports_steering ----------------------------------------

    #[tokio::test]
    async fn driver_steers_mid_session_and_mock_observes_payload() {
        let session = spawn_with_handshake(mock_command("steering"), None)
            .await
            .expect("spawn");
        let mut session = session.prompt("first prompt").await.expect("prompt ok");

        // Drain events from the first turn until turn_end so the session is
        // ready for a steer.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::TurnEnd) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before first TurnEnd"),
            }
        }

        session
            .steer("STEERED_TEXT")
            .await
            .expect("steer should succeed");

        let mut saw_steer_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::MessageDelta { text }) => {
                    if text.contains("STEERED_TEXT") {
                        saw_steer_echo = true;
                    }
                }
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(saw_steer_echo, "mock did not observe the steer payload");
    }

    // -- test_pi_compaction_repin -----------------------------------------

    #[tokio::test]
    async fn driver_repins_on_compaction_start_via_steer() {
        let session = spawn_with_handshake(mock_command("compaction"), None)
            .await
            .expect("spawn");
        let repin_text = "REPIN_PAYLOAD_TEXT";
        let mut session = session.prompt("kickoff").await.expect("prompt ok");

        // Drive events until CompactionStart arrives — at that point the
        // workflow layer (here represented by the test) sends a steer
        // carrying the re-pin payload.
        let mut sent_repin = false;
        let mut saw_repin_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::CompactionStart { .. }) if !sent_repin => {
                    session.steer(repin_text).await.expect("steer ok");
                    sent_repin = true;
                }
                Some(AgentEvent::MessageDelta { text }) => {
                    if text.contains(repin_text) {
                        saw_repin_echo = true;
                    }
                }
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(sent_repin, "compaction_start was not observed");
        assert!(saw_repin_echo, "mock did not echo the re-pin payload");
    }

    // -- test_pi_set_model_from_phase_config ------------------------------

    #[tokio::test]
    async fn set_model_from_phase_config_reaches_mock_pi() {
        let model = ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        };
        let session = spawn_with_handshake(mock_command("set-model"), Some(&model))
            .await
            .expect("spawn with model");

        // The mock echoes provider/modelId via a MessageDelta on the first
        // prompt so the test can assert the values reached pi.
        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_provider = false;
        let mut saw_model_id = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(AgentEvent::MessageDelta { text }) => {
                    if text.contains("deepseek") {
                        saw_provider = true;
                    }
                    if text.contains("deepseek-v3") {
                        saw_model_id = true;
                    }
                }
                Some(AgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_provider, "mock did not observe provider");
        assert!(saw_model_id, "mock did not observe model_id");
    }
}
