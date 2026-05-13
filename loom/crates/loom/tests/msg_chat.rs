//! R6 (wx-ibgar) — `loom msg --chat` integration tests.
//!
//! Each test drives the mock pi agent through a distinct chat-mode
//! scenario and asserts the resulting bd state / driver output:
//!
//! - `launches_container` — the mock-loom-agent's protocol handshake
//!   shows up in the bd-shim invocation log (proof the dispatch reached
//!   the agent and the agent completed cleanly).
//! - `writes_notes` — `chat-resolve-all` mode shells out to
//!   `bd update <id> --notes "…" --remove-label=loom:clarify` per
//!   clarify; the bd-shim invocation log carries the calls and the
//!   bead state reflects the update.
//! - `partial_progress` — `chat-resolve-none` emits LOOM_COMPLETE
//!   without resolving anything; remaining clarifies persist for the
//!   next session.
//! - `rejects_non_complete_exit` — `chat-emit-blocked` makes the agent
//!   emit `LOOM_BLOCKED`; loom msg --chat refuses (non-zero exit).
//! - `scope_filters_to_spec` — `-s <label>` narrows the rendered
//!   prompt to that spec's clarifies; the mock dumps the prompt and
//!   the test asserts only the in-scope IDs are present.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn seed_bead(state_dir: &Path, id: &str, title: &str, description: &str, labels: &[&str]) {
    let bead_dir = state_dir.join(id);
    std::fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    std::fs::write(bead_dir.join("title"), title).expect("write title");
    std::fs::write(bead_dir.join("description"), description).expect("write description");
    std::fs::write(bead_dir.join("status"), "open").expect("write status");
    std::fs::write(bead_dir.join("priority"), "2").expect("write priority");
    std::fs::write(bead_dir.join("issue_type"), "task").expect("write issue_type");
    let body = labels.join("\n");
    std::fs::write(bead_dir.join("labels"), body).expect("write labels");
}

fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(_) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

fn write_minimal_manifest(dir: &Path) -> PathBuf {
    let source = dir.join("base.tar");
    std::fs::write(&source, "").expect("write base.tar");
    let manifest = dir.join("profile-images.json");
    let body = format!(
        r#"{{"base": {{"ref":"localhost/wrapix-base:test","source":{source:?}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

struct ChatRun {
    workspace: PathBuf,
    state_dir: PathBuf,
    bin_dir: PathBuf,
    manifest: PathBuf,
    _tmp: tempfile::TempDir,
}

fn setup_chat() -> ChatRun {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().to_path_buf();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    let bin_dir = install_bd_shim(&workspace);
    let manifest = write_minimal_manifest(&workspace);
    ChatRun {
        workspace,
        state_dir,
        bin_dir,
        manifest,
        _tmp: tmp,
    }
}

fn run_loom_msg_chat(env: &ChatRun, mode: &str, args: &[&str]) -> std::process::Output {
    run_loom_msg_chat_with_extra_env(env, mode, args, &[])
}

fn run_loom_msg_chat_with_extra_env(
    env: &ChatRun,
    mode: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![env.bin_dir.clone()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let mock_agent = env!("CARGO_BIN_EXE_mock-loom-agent");

    let mut cmd = Command::new(loom_bin);
    cmd.arg("--workspace")
        .arg(&env.workspace)
        .arg("--agent")
        .arg("pi")
        .arg("msg")
        .arg("-c")
        .args(args)
        .env("PATH", new_path)
        .env("LOOM_WRAPIX_BIN", mock_agent)
        .env("LOOM_TEST_AGENT_MODE", mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &env.manifest)
        .env("BD_STATE_DIR", &env.state_dir)
        .env("XDG_STATE_HOME", env.workspace.join(".loom-test-state"));
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn loom")
}

fn read_invocation_log(state_dir: &Path) -> String {
    std::fs::read_to_string(state_dir.join(".invocations.log")).unwrap_or_default()
}

fn read_field(state_dir: &Path, id: &str, field: &str) -> String {
    std::fs::read_to_string(state_dir.join(id).join(field)).unwrap_or_default()
}

fn read_labels(state_dir: &Path, id: &str) -> Vec<String> {
    read_field(state_dir, id, "labels")
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn loom_msg_chat_launches_container() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-c01",
        "container launch pin",
        "## Options — pick one\n\n### Option 1 — A\nbody\n",
        &["loom:clarify", "spec:scope-a"],
    );
    let output = run_loom_msg_chat(&env, "chat-resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 on a clean session.\nstdout={stdout}\nstderr={stderr}",
    );
    // The dispatch path opened bd to list clarifies — the bd-shim
    // invocation log carries that list call. The presence of the
    // session marker proves the agent ran end-to-end.
    let log = read_invocation_log(&env.state_dir);
    assert!(
        log.contains("list") || log.contains("show"),
        "expected at least one bd invocation from the dispatch path: {log:?}",
    );
    assert!(
        stdout.contains("loom msg --chat"),
        "expected a session-summary line on stdout: {stdout:?}",
    );
}

#[test]
fn loom_msg_chat_writes_notes_and_clears_labels() {
    let env = setup_chat();
    for id in ["wx-w01", "wx-w02", "wx-w03"] {
        seed_bead(
            &env.state_dir,
            id,
            &format!("note-pin {id}"),
            "## Options — choose\n\n### Option 1 — only\nbody\n",
            &["loom:clarify", "spec:notes"],
        );
    }
    let output = run_loom_msg_chat(&env, "chat-resolve-all", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 when chat-resolve-all completes.\n\
         stdout={stdout}\nstderr={stderr}",
    );
    let log = read_invocation_log(&env.state_dir);
    for id in ["wx-w01", "wx-w02", "wx-w03"] {
        assert!(
            log.contains(&format!("update {id}")),
            "expected bd update call for {id}: {log}",
        );
        let notes = read_field(&env.state_dir, id, "notes");
        assert!(
            notes.contains("resolved via msg --chat"),
            "bead {id} notes not updated: {notes:?}",
        );
        let labels = read_labels(&env.state_dir, id);
        assert!(
            !labels.iter().any(|l| l == "loom:clarify"),
            "bead {id} should have lost loom:clarify label: {labels:?}",
        );
    }
}

#[test]
fn loom_msg_chat_partial_progress_leaves_unresolved_clarifies_open() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-p01",
        "partial",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:partial"],
    );
    let output = run_loom_msg_chat(&env, "chat-resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "partial-progress session must exit 0 (clean per spec).\n\
         stdout={stdout}\nstderr={stderr}",
    );
    let labels = read_labels(&env.state_dir, "wx-p01");
    assert!(
        labels.iter().any(|l| l == "loom:clarify"),
        "unresolved bead must keep loom:clarify: {labels:?}",
    );
    // Notes column stays empty — the mock didn't touch it.
    let notes = read_field(&env.state_dir, "wx-p01", "notes");
    assert!(
        notes.is_empty(),
        "unresolved bead notes should be empty: {notes:?}",
    );
}

#[test]
fn loom_msg_chat_rejects_non_complete_exit_signal() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-x01",
        "exit-signal",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:exit"],
    );
    let output = run_loom_msg_chat(&env, "chat-emit-blocked", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "LOOM_BLOCKED from chat must fail the session: stderr={stderr}",
    );
    assert!(
        stderr.contains("LOOM_BLOCKED") || stderr.contains("clarifies are resolved"),
        "error must explain the rejection: stderr={stderr}",
    );
}

#[test]
fn loom_msg_chat_scope_filters_to_spec() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-s01",
        "in-scope alpha",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &env.state_dir,
        "wx-s02",
        "out-of-scope beta",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:beta"],
    );
    let prompt_dump = env.workspace.join("prompt-dump.txt");
    let output = run_loom_msg_chat_with_extra_env(
        &env,
        "chat-prompt-dump",
        &["-s", "alpha"],
        &[("LOOM_TEST_PROMPT_DUMP", &prompt_dump.to_string_lossy())],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "scope filter session must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    let dumped = std::fs::read_to_string(&prompt_dump)
        .unwrap_or_else(|e| panic!("read prompt dump {}: {e}", prompt_dump.display()));
    assert!(
        dumped.contains("wx-s01"),
        "in-scope bead must appear in prompt: {dumped:.500?}",
    );
    assert!(
        !dumped.contains("wx-s02"),
        "out-of-scope bead must NOT appear in prompt: {dumped:.500?}",
    );
}
