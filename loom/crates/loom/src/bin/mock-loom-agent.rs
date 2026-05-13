//! Test-only mock agent. Stands in for the wrapix → pi binary chain
//! when an integration test wants to drive `loom run` end-to-end
//! against a controllable backend. Speaks the pi-mono JSONL RPC
//! protocol — enough of it to reach the marker-emission point and
//! exit cleanly — and chooses what marker text to emit based on the
//! `LOOM_TEST_AGENT_MODE` env var.
//!
//! Selection: the test points `LOOM_WRAPIX_BIN` at this binary. loom
//! invokes it with `spawn --spawn-config <path> --stdio` (the
//! production wrapix CLI shape) plus whatever spawn-config the
//! production code resolved. The mock ignores those args — there is
//! no container, no wrapper — and immediately starts speaking pi-mono
//! on its own stdin/stdout. That collapses two test-harness layers
//! into one Rust binary, replacing the previous bash `install_wrapix_shim`
//! + `tests/loom/mock-pi/pi.sh` pair without extending either.
//!
//! Modes (set `LOOM_TEST_AGENT_MODE`):
//!
//! - `blocked-marker`  — emit `<reason>\nLOOM_BLOCKED` and `agent_end`.
//!   Drives the run gate's `AgentOutcome::Blocked` branch.
//! - `clarify-marker`  — emit `<question>\nLOOM_CLARIFY` and `agent_end`.
//!   Drives the run gate's `AgentOutcome::Clarify` branch.
//! - `complete-marker` — emit `done\nLOOM_COMPLETE` and `agent_end`.
//!   Used by the negative case in B6: the agent should also call
//!   `bd close` itself, but the test asserts the *driver* doesn't.
//! - `no-marker`       — emit a plain message and `agent_end` without
//!   any `LOOM_*` line. Exercises the swallowed-marker recovery path.
//! - `chat-resolve-all` — R6 (wx-ibgar). Parse the received prompt for
//!   `### <bead-id> — …` lines, fork `bd update <id> --notes "resolved" \
//!   --remove-label=loom:clarify` for each, then emit `LOOM_COMPLETE`.
//!   Tests assert the bd-shim invocation log carries the right calls.
//! - `chat-resolve-none` — R6. Emit `LOOM_COMPLETE` without resolving
//!   anything; exercises the "partial-progress is clean" path.
//! - `chat-emit-blocked` — R6. Emit `LOOM_BLOCKED` so the driver
//!   rejects the session (only `LOOM_COMPLETE` is valid for msg).
//! - `chat-prompt-dump` — R6. Write the received prompt verbatim to
//!   `$LOOM_TEST_PROMPT_DUMP`, then `LOOM_COMPLETE`. Tests inspect the
//!   dump to assert scope filtering and template rendering.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helper: loud failure on protocol violations is the desired behaviour"
)]

use std::env;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

const MODE_BLOCKED: &str = "blocked-marker";
const MODE_CLARIFY: &str = "clarify-marker";
const MODE_COMPLETE: &str = "complete-marker";
const MODE_NO_MARKER: &str = "no-marker";
const MODE_CHAT_RESOLVE_ALL: &str = "chat-resolve-all";
const MODE_CHAT_RESOLVE_NONE: &str = "chat-resolve-none";
const MODE_CHAT_EMIT_BLOCKED: &str = "chat-emit-blocked";
const MODE_CHAT_PROMPT_DUMP: &str = "chat-prompt-dump";

const BLOCKED_REASON: &str = "spec section is missing the schema for this bead";
const CLARIFY_QUESTION: &str = "which deploy-key path should the runner image mount?";

fn main() -> ExitCode {
    let mode = env::var("LOOM_TEST_AGENT_MODE").unwrap_or_else(|_| MODE_COMPLETE.to_string());

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Step 1 — handshake. Production loom sends a `get_commands` probe
    // as the first JSONL line; the agent must respond with the full
    // command list before any further protocol traffic flows. Pi-mono's
    // unit tests in loom-agent assert this exact set of commands.
    let probe_line = match read_line(&stdin) {
        Some(l) => l,
        None => {
            eprintln!("mock-loom-agent: stdin closed before probe");
            return ExitCode::from(2);
        }
    };
    let probe_id = extract_field("id", &probe_line).unwrap_or_else(|| "probe-0".to_string());
    let probe_response = format!(
        r#"{{"type":"response","id":"{probe_id}","command":"get_commands","success":true,"data":["prompt","steer","abort","set_model","compact","get_session_stats"]}}"#,
    );
    emit(&mut stdout, &probe_response);

    // Step 2 — read the prompt line. R6 chat modes parse it for the
    // outstanding clarify bead IDs; non-chat modes ignore it.
    let prompt_line = read_line(&stdin).unwrap_or_default();
    let prompt_text = extract_prompt_message(&prompt_line);

    // Step 3 — emit the marker as a message_delta. parse_exit_signal
    // scans the accumulated text for `LOOM_BLOCKED` / `LOOM_CLARIFY` /
    // `LOOM_COMPLETE` markers; emitting the reason on the prior line
    // matches `reason_for`'s "non-empty line before the marker" rule.
    match mode.as_str() {
        MODE_BLOCKED => {
            emit_message_delta(&mut stdout, BLOCKED_REASON);
            emit_message_delta(&mut stdout, "LOOM_BLOCKED");
        }
        MODE_CLARIFY => {
            emit_message_delta(&mut stdout, CLARIFY_QUESTION);
            emit_message_delta(&mut stdout, "LOOM_CLARIFY");
        }
        MODE_COMPLETE => {
            emit_message_delta(&mut stdout, "did the work");
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        MODE_NO_MARKER => {
            emit_message_delta(&mut stdout, "ran without emitting a verdict");
        }
        MODE_CHAT_RESOLVE_ALL => {
            // R6 — walk every clarify the prompt enumerates and shell
            // out to `bd update` for each. `bd` resolves via PATH so the
            // test must place a `bd-shim` on PATH (the existing
            // marker_gate / msg_persist helpers already do this).
            let ids = extract_clarify_bead_ids(&prompt_text);
            for id in &ids {
                let mut update_args = vec![
                    "update".to_string(),
                    id.clone(),
                    "--notes".to_string(),
                    format!("resolved via msg --chat (mock {id})"),
                    "--remove-label".to_string(),
                    "loom:clarify".to_string(),
                ];
                shell_out_bd(&mut update_args);
            }
            emit_message_delta(
                &mut stdout,
                &format!("resolved {} clarify bead(s)", ids.len()),
            );
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        MODE_CHAT_RESOLVE_NONE => {
            emit_message_delta(&mut stdout, "user exited early; no clarifies resolved");
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        MODE_CHAT_EMIT_BLOCKED => {
            emit_message_delta(&mut stdout, "mock cannot resolve any clarifies");
            emit_message_delta(&mut stdout, "LOOM_BLOCKED");
        }
        MODE_CHAT_PROMPT_DUMP => {
            // Dump the prompt verbatim so the test can assert what the
            // renderer produced (scope filtering, template shape).
            if let Ok(path) = env::var("LOOM_TEST_PROMPT_DUMP") {
                let _ = std::fs::write(&path, &prompt_text);
            }
            emit_message_delta(&mut stdout, "prompt dumped");
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        other => {
            eprintln!("mock-loom-agent: unknown LOOM_TEST_AGENT_MODE {other}");
            return ExitCode::from(2);
        }
    }

    // Step 4 — close the session. agent_end terminates the pi-mono
    // event stream; the driver-side loop reads up to that marker and
    // moves on. Exit 0 unconditionally — the verdict comes from the
    // marker text, not the exit code.
    emit(&mut stdout, r#"{"type":"agent_end","messages":[]}"#);
    ExitCode::SUCCESS
}

fn read_line(stdin: &io::Stdin) -> Option<String> {
    let mut buf = String::new();
    let n = stdin.lock().read_line(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    Some(buf)
}

fn emit(stdout: &mut io::Stdout, line: &str) {
    writeln!(stdout, "{line}").expect("emit line");
    stdout.flush().expect("flush stdout");
}

fn emit_message_delta(stdout: &mut io::Stdout, text: &str) {
    let event = serde_json::json!({
        "type": "message_update",
        "assistantMessageEvent": {
            "type": "text_delta",
            "text": text,
        },
    });
    emit(stdout, &event.to_string());
}

/// Minimal JSON field extractor — pulls a string value for `field` out
/// of a flat JSON line. The probe line is shallow (`{"type":"command",
/// "id":"…","name":"get_commands"}`) so this avoids pulling serde_json's
/// parser into the read path; if the field is absent we fall back to a
/// stand-in id and the caller decides whether that's fatal.
fn extract_field(field: &str, line: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Pull the prompt body out of pi's `prompt` command line. Pi wraps the
/// user-facing prompt in `{"type":"command","name":"prompt","args":{
/// "message":"<body>"}}` — well, in practice with the wrapix wrapper
/// the layout differs slightly. We do a tolerant parse: find the first
/// `"message":` field, then JSON-decode its string value. R6 chat
/// modes work on the decoded plaintext.
fn extract_prompt_message(line: &str) -> String {
    let Some(start) = line.find("\"message\":\"") else {
        return String::new();
    };
    let body_start = start + "\"message\":\"".len();
    let mut out = String::new();
    let mut chars = line[body_start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

/// Parse the rendered msg.md prompt for `### <bead-id> — [spec:…] <title>`
/// lines and return every bead id in source order. The template emits
/// one per outstanding clarify bead; this is how the mock walks the
/// queue without bouncing through bd-list itself.
fn extract_clarify_bead_ids(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim_start();
        let Some(after_hash) = trimmed.strip_prefix("### ") else {
            continue;
        };
        // The bead id is the first whitespace-delimited token after `### `.
        // A trailing `—` may abut without a space when titles are tight,
        // so split on ASCII whitespace and accept the first non-empty.
        let id = after_hash
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(',');
        if id.starts_with("wx-") || id.starts_with("bd-") {
            out.push(id.to_string());
        }
    }
    out
}

/// Shell out to `bd <args>` so the chat-resolve-all mode can write
/// notes + clear labels through the same bd surface a real claude
/// session would. PATH lookup finds whatever `bd` (or `bd-shim` in
/// tests) the parent installed. Failures are reported on stderr but
/// don't fail the session — the integration test asserts the
/// invocation log, not the exit code of every individual update.
fn shell_out_bd(args: &mut [String]) {
    use std::process::Command;
    let result = Command::new("bd")
        .args(args.iter().map(String::as_str))
        .output();
    match result {
        Ok(out) if !out.status.success() => {
            eprintln!(
                "mock-loom-agent: bd {:?} exited {} stderr={}",
                args,
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr),
            );
        }
        Err(e) => eprintln!("mock-loom-agent: bd spawn failed: {e}"),
        _ => {}
    }
}
