//! Pi-mono RPC backend: spawn + startup probe + optional `set_model`.
//!
//! [`PiBackend::spawn`] serializes the [`SpawnConfig`] to a JSON file,
//! execs `wrapix spawn --spawn-config <file> --stdio` (the wrapper that
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
use std::time::Duration;

use loom_driver::agent::{
    Active, AgentBackend, AgentSession, DEFAULT_HANDSHAKE_TIMEOUT_SECS, Idle, JsonlReader,
    ModelSelection, ProtocolError, SpawnConfig,
};
use loom_driver::clock::{Clock, SystemClock};
use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::{ChildStdin, Command};
use tracing::{debug, error, info, warn};

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
        let handshake_budget = config
            .handshake_timeout
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS));
        info!(
            wrapix = %wrapix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            handshake_timeout_secs = handshake_budget.as_secs(),
            "pi backend spawn",
        );

        let mut cmd = Command::new(&wrapix_bin);
        cmd.arg("spawn")
            .arg("--spawn-config")
            .arg(&spawn_config_path)
            .arg("--stdio");

        spawn_with_handshake(
            cmd,
            config.model.as_ref(),
            handshake_budget,
            &SystemClock::new(),
        )
        .await
    }

    async fn on_compaction_start(
        session: &mut AgentSession<Active>,
        config: &SpawnConfig,
    ) -> Result<(), ProtocolError> {
        debug!(
            scratch_dir = %config.scratch_dir.display(),
            "pi compaction_start observed; reading scratch dir for re-pin payload",
        );
        let payload = build_repin_payload(&config.scratch_dir)?;
        session.steer(&payload).await
    }
}

/// Read `prompt.txt` and `scratch.md` from the per-session scratch dir and
/// concatenate them into the `steer` payload. Same source files Claude
/// reads through `repin.sh` (see [`ScratchSession`]'s `repin.sh` in
/// `loom-driver/src/scratch.rs`); pi's transport is `steer` rather than a
/// JSON envelope, but the text content matches.
///
/// [`ScratchSession`]: loom_driver::scratch::ScratchSession
fn build_repin_payload(scratch_dir: &Path) -> Result<String, ProtocolError> {
    let prompt = std::fs::read_to_string(scratch_dir.join("prompt.txt"))?;
    let scratch = std::fs::read_to_string(scratch_dir.join("scratch.md"))?;
    Ok(format!("{prompt}\n\n{scratch}"))
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
/// Public so integration tests under `loom-agent/tests/` can substitute a
/// mock pi binary in place of the real `wrapix spawn` exec without going
/// through the `LOOM_WRAPIX_BIN` env-var override (Rust 2024 makes
/// `env::set_var` unsafe, and the workspace forbids `unsafe_code`).
/// Production callers go through [`PiBackend::spawn`].
pub async fn spawn_with_handshake(
    mut cmd: Command,
    model: Option<&ModelSelection>,
    handshake_timeout: Duration,
    clock: &dyn Clock,
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
    let mut reader = JsonlReader::new(stdout);

    run_probe(&mut writer, &mut reader, handshake_timeout, clock).await?;

    if let Some(model) = model {
        run_set_model(&mut writer, &mut reader, model, handshake_timeout, clock).await?;
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
    reader: &mut JsonlReader,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let cmd = GetCommandsCommand {
        kind: "get_commands",
        id: PROBE_REQUEST_ID,
    };
    info!(id = PROBE_REQUEST_ID, "pi probe: sending get_commands");
    write_command(writer, &cmd).await?;

    let resp = bounded_await_response(reader, PROBE_REQUEST_ID, budget, "probe", clock).await?;
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

    info!(commands = ?commands, "pi probe: get_commands succeeded");
    Ok(())
}

/// Send `set_model` on stdin and wait for the matching response. A failure
/// response is a hard fail — Loom requires the requested model to take
/// effect before the workflow begins.
async fn run_set_model(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut JsonlReader,
    model: &ModelSelection,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let cmd = SetModelCommand {
        kind: "set_model",
        id: SET_MODEL_REQUEST_ID,
        provider: &model.provider,
        model_id: &model.model_id,
    };
    info!(
        id = SET_MODEL_REQUEST_ID,
        provider = %model.provider,
        model_id = %model.model_id,
        "pi handshake: sending set_model",
    );
    write_command(writer, &cmd).await?;

    let resp =
        bounded_await_response(reader, SET_MODEL_REQUEST_ID, budget, "set_model", clock).await?;
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

/// Encode `payload` as JSONL and flush it to pi's stdin.
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

/// Read JSONL lines until one classifies as the `response` matching
/// `expected_id`. Other lines (events, unrelated responses, extension UI
/// requests) are observed and dropped — request/response correlation is
/// the only contract this loop enforces.
async fn await_response(
    reader: &mut JsonlReader,
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

/// [`await_response`] with a [`Clock`]-driven budget. Surfaces
/// [`ProtocolError::HandshakeTimeout`] when the budget elapses so loom
/// breaks out of a non-responsive pi process instead of blocking forever.
/// The reader is *not* re-used after timeout — the connection is torn down
/// by the caller (`spawn_with_handshake` returns the error and the child
/// drops, which kills the process via `kill_on_drop`). Uses
/// `clock.sleep(...)` in a `tokio::select!` rather than `Clock::timeout`
/// because the trait object surface (`&dyn Clock`) is not `Sized`, but
/// `Clock::timeout` carries a `Self: Sized` bound.
async fn bounded_await_response(
    reader: &mut JsonlReader,
    expected_id: &str,
    budget: Duration,
    stage: &'static str,
    clock: &dyn Clock,
) -> Result<PiResponse, ProtocolError> {
    let response = await_response(reader, expected_id);
    let sleep = clock.sleep(budget);
    tokio::select! {
        result = response => result,
        () = sleep => {
            warn!(
                stage,
                budget_secs = budget.as_secs(),
                "pi handshake timed out — agent process did not reply",
            );
            Err(ProtocolError::HandshakeTimeout {
                stage,
                after: budget,
            })
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
/// under the system temp dir. The path is handed to `wrapix spawn
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
    use loom_driver::agent::RePinContent;
    use loom_events::ParsedAgentEvent;
    use std::path::PathBuf;

    fn mock_pi_path() -> PathBuf {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            let candidate = ancestor.join("tests/loom/mock-pi/pi.sh");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!(
            "could not locate tests/loom/mock-pi/pi.sh above {} — neither \
             dev-tree nor nix-sandbox layout matched.",
            manifest_dir.display(),
        );
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

    /// Mock-pi scenarios all reply within ~50ms; 5 s is comfortably above
    /// any legitimate jitter without making a hung scenario stall the suite.
    const TEST_HANDSHAKE_BUDGET: Duration = Duration::from_secs(5);

    fn sample_config(model: Option<ModelSelection>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrapix-test:pi".to_string(),
            image_source: PathBuf::from("/nix/store/zzz-wrapix-test-pi.tar"),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRAPIX_AGENT".into(), "pi".into())],
            initial_prompt: "hello pi".to_string(),
            agent_args: vec![],
            repin: sample_repin(),
            scratch_dir: PathBuf::from("/workspace/.wrapix/loom/scratch/test"),
            model,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: None,
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
        assert_eq!(decoded.image_ref, cfg.image_ref);
        assert_eq!(decoded.image_source, cfg.image_source);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        let model = decoded.model.expect("model present");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.model_id, "deepseek-v3");
        let _ = std::fs::remove_file(&path);
    }

    // -- test_pi_startup_probe --------------------------------------------

    #[tokio::test]
    async fn startup_probe_succeeds_when_required_commands_present() {
        let session = spawn_with_handshake(
            mock_command("happy-path"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("probe should succeed");
        // Drive a prompt to confirm the session is wired and the mock keeps
        // running past the probe.
        let mut session = session.prompt("ping").await.expect("prompt ok");
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
    }

    #[tokio::test]
    async fn startup_probe_fails_fast_when_required_command_missing() {
        let result = spawn_with_handshake(
            mock_command("probe-missing-set-model"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await;
        match result {
            Err(ProtocolError::Unsupported) => {}
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("probe should have failed"),
        }
    }

    // -- test_pi_rpc_command_sending --------------------------------------

    #[tokio::test]
    async fn driver_sends_prompt_as_jsonl_line() {
        let session = spawn_with_handshake(
            mock_command("echo-prompt"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("HELLO_PROMPT").await.expect("prompt ok");

        let mut saw_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("HELLO_PROMPT") {
                        saw_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_echo, "mock did not echo the prompt");
    }

    // -- test_pi_supports_steering ----------------------------------------

    #[tokio::test]
    async fn driver_steers_mid_session_and_mock_observes_payload() {
        let session = spawn_with_handshake(
            mock_command("steering"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("first prompt").await.expect("prompt ok");

        // Drain events from the first turn until turn_end so the session is
        // ready for a steer.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TurnEnd) => break,
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
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("STEERED_TEXT") {
                        saw_steer_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(saw_steer_echo, "mock did not observe the steer payload");
    }

    // -- test_pi_compaction_repin -----------------------------------------

    #[tokio::test]
    async fn driver_repins_on_compaction_start_via_steer() {
        let session = spawn_with_handshake(
            mock_command("compaction"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
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
                Some(ParsedAgentEvent::CompactionStart { .. }) if !sent_repin => {
                    session.steer(repin_text).await.expect("steer ok");
                    sent_repin = true;
                }
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains(repin_text) {
                        saw_repin_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(sent_repin, "compaction_start was not observed");
        assert!(saw_repin_echo, "mock did not echo the re-pin payload");
    }

    #[tokio::test]
    async fn on_compaction_start_steers_concatenated_scratch_files() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::write(scratch.path().join("prompt.txt"), "PROMPT_FILE_BODY")
            .expect("write prompt.txt");
        std::fs::write(scratch.path().join("scratch.md"), "SCRATCH_FILE_BODY")
            .expect("write scratch.md");

        let mut config = sample_config(None);
        config.scratch_dir = scratch.path().to_path_buf();

        let session = spawn_with_handshake(
            mock_command("compaction"),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("kickoff").await.expect("prompt ok");

        let mut handler_called = false;
        let mut saw_prompt_echo = false;
        let mut saw_scratch_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::CompactionStart { .. }) if !handler_called => {
                    PiBackend::on_compaction_start(&mut session, &config)
                        .await
                        .expect("on_compaction_start ok");
                    handler_called = true;
                }
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("PROMPT_FILE_BODY") {
                        saw_prompt_echo = true;
                    }
                    if text.contains("SCRATCH_FILE_BODY") {
                        saw_scratch_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(handler_called, "compaction_start was not observed");
        assert!(
            saw_prompt_echo,
            "prompt.txt content missing from steer payload"
        );
        assert!(
            saw_scratch_echo,
            "scratch.md content missing from steer payload"
        );
    }

    #[test]
    fn build_repin_payload_concatenates_prompt_then_scratch() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("prompt.txt"), "the prompt").expect("write prompt.txt");
        std::fs::write(dir.path().join("scratch.md"), "the scratch").expect("write scratch.md");
        let payload = build_repin_payload(dir.path()).expect("build payload");
        assert_eq!(payload, "the prompt\n\nthe scratch");
    }

    #[test]
    fn build_repin_payload_surfaces_io_error_when_files_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = build_repin_payload(dir.path()).expect_err("missing files must error");
        match err {
            ProtocolError::Io(_) => {}
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    // -- test_pi_set_model_from_phase_config ------------------------------

    #[tokio::test]
    async fn set_model_from_phase_config_reaches_mock_pi() {
        let model = ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        };
        let session = spawn_with_handshake(
            mock_command("set-model"),
            Some(&model),
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn with model");

        // The mock echoes provider/modelId via a MessageDelta on the first
        // prompt so the test can assert the values reached pi.
        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_provider = false;
        let mut saw_model_id = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("deepseek") {
                        saw_provider = true;
                    }
                    if text.contains("deepseek-v3") {
                        saw_model_id = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_provider, "mock did not observe provider");
        assert!(saw_model_id, "mock did not observe model_id");
    }
}
