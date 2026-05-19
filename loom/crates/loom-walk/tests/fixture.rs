//! Integration tests for the `loom-walk` binary: dispatcher contract +
//! per-walk pass/fail fixtures.
//!
//! Each registered walk gets a pair of subprocess tests: synthesise
//! source under `tempfile::tempdir`, set CWD + `LOOM_FILES` on the
//! invocation, and assert verdict + exit code per the verifier-runner
//! contract in `specs/loom-gate.md`.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Invoke the built `loom-walk` binary with argv, CWD, and `LOOM_FILES`.
/// The dispatcher's contract is process-shaped (env in, JSON on
/// stdout, exit code) so subprocess invocation is the test surface.
fn invoke(args: &[&str], cwd: Option<&Path>, loom_files: Option<&str>) -> Output {
    let bin = env!("CARGO_BIN_EXE_loom-walk");
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    match loom_files {
        Some(value) => {
            cmd.env("LOOM_FILES", value);
        }
        None => {
            cmd.env_remove("LOOM_FILES");
        }
    }
    cmd.output().unwrap()
}

/// Build a minimal workspace tree (`Cargo.toml` with the marker, plus
/// the crates the caller seeds) under a tempdir so the walks'
/// `workspace_root()` detection points at the tempdir.
fn make_workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let cargo = "[workspace]\n\
                 resolver = \"3\"\n\
                 members = [\"crates/loom-driver\"]\n\
                 \n\
                 [workspace.package]\n\
                 edition = \"2024\"\n";
    std::fs::write(dir.path().join("Cargo.toml"), cargo).unwrap();
    dir
}

fn seed(root: &Path, rel: &str, body: &str) -> PathBuf {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, body).unwrap();
    full
}

fn parse_verdict(out: &Output) -> (Value, i32) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (v, out.status.code().unwrap())
}

fn assert_pass(out: &Output) {
    let (v, code) = parse_verdict(out);
    assert!(
        v["pass"].as_bool().unwrap(),
        "expected pass, got {v:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "expected exit 0, got {code}");
}

fn assert_fail(out: &Output, evidence_contains: &str) {
    let (v, code) = parse_verdict(out);
    assert!(
        !v["pass"].as_bool().unwrap(),
        "expected fail, got {v:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 1, "expected exit 1, got {code}");
    let evidence = v["evidence"].as_str().unwrap();
    assert!(
        evidence.contains(evidence_contains),
        "evidence missing fragment `{evidence_contains}`:\n{evidence}"
    );
}

// ---------------------------------------------------------------------------
// Dispatcher contract
// ---------------------------------------------------------------------------

#[test]
fn missing_walk_name_exits_two_and_names_available_walks() {
    let out = invoke(&[], None, None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: loom-walk"), "stderr={stderr}");
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

#[test]
fn unknown_walk_name_exits_two_and_names_the_walk_and_available_set() {
    let out = invoke(&["definitely_not_a_walk"], None, None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown walk"),
        "must say 'unknown walk'; stderr={stderr}"
    );
    assert!(
        stderr.contains("definitely_not_a_walk"),
        "must echo the offending walk name; stderr={stderr}"
    );
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

// ---------------------------------------------------------------------------
// no_derive_from_on_newtypes (RS-8)
// ---------------------------------------------------------------------------

#[test]
fn no_derive_from_on_newtypes_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub struct Id(pub u32);\n#[derive(Clone)]\npub struct Token(pub String);\n",
    );
    let out = invoke(
        &["no_derive_from_on_newtypes"],
        Some(ws.path()),
        Some(
            &ws.path()
                .join("crates/loom-driver/src/lib.rs")
                .to_string_lossy(),
        ),
    );
    assert_pass(&out);
}

#[test]
fn no_derive_from_on_newtypes_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "#[derive(From)]\npub struct Id(pub u32);\n",
    );
    let out = invoke(
        &["no_derive_from_on_newtypes"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "derive(From)");
}

// ---------------------------------------------------------------------------
// no_types_or_error_files (RS-5)
// ---------------------------------------------------------------------------

#[test]
fn no_types_or_error_files_pass() {
    let ws = make_workspace();
    seed(ws.path(), "crates/loom-driver/src/lib.rs", "pub mod foo;\n");
    seed(
        ws.path(),
        "crates/loom-driver/src/foo/mod.rs",
        "pub mod types;\n",
    );
    let out = invoke(&["no_types_or_error_files"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn no_types_or_error_files_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub mod types;\n",
    );
    seed(
        ws.path(),
        "crates/loom-driver/src/types.rs",
        "pub struct X;\n",
    );
    let out = invoke(&["no_types_or_error_files"], Some(ws.path()), None);
    assert_fail(&out, "types.rs");
}

// ---------------------------------------------------------------------------
// git_client_encapsulation
// ---------------------------------------------------------------------------

#[test]
fn git_client_encapsulation_pass() {
    let ws = make_workspace();
    let allowed = seed(
        ws.path(),
        "crates/loom-driver/src/git/mod.rs",
        "use gix::Repository;\npub fn check() -> Repository { todo!() }\n",
    );
    let outside = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub fn nothing() {}\n",
    );
    let scope = format!(
        "{}:{}",
        allowed.to_string_lossy(),
        outside.to_string_lossy()
    );
    let out = invoke(&["git_client_encapsulation"], Some(ws.path()), Some(&scope));
    assert_pass(&out);
}

#[test]
fn git_client_encapsulation_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "use gix::Repository;\n",
    );
    let out = invoke(
        &["git_client_encapsulation"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "gix");
}

// ---------------------------------------------------------------------------
// single_event_channel
// ---------------------------------------------------------------------------

#[test]
fn single_event_channel_pass() {
    let ws = make_workspace();
    let sink = "pub struct LogSink { file: std::fs::File, renderer: Renderer }\n\
                impl LogSink {\n\
                    pub fn emit(&mut self, ev: Event) {\n\
                        self.file.write_all(b\"\").unwrap();\n\
                        self.renderer.render(ev);\n\
                    }\n\
                }\n";
    seed(ws.path(), "crates/loom-render/src/sink/mod.rs", sink);
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn single_event_channel_fail_missing_emit() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/src/sink/mod.rs",
        "pub struct LogSink;\nimpl LogSink { pub fn new() -> Self { Self } }\n",
    );
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_fail(&out, "LogSink::emit method not found");
}

#[test]
fn single_event_channel_fail_emit_misses_file() {
    let ws = make_workspace();
    let sink = "pub struct LogSink { renderer: Renderer }\n\
                impl LogSink {\n\
                    pub fn emit(&mut self, ev: Event) { self.renderer.render(ev); }\n\
                }\n";
    seed(ws.path(), "crates/loom-render/src/sink/mod.rs", sink);
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_fail(&out, "self.file");
}

// ---------------------------------------------------------------------------
// newtype_identifiers (RS-7)
// ---------------------------------------------------------------------------

#[test]
fn newtype_identifiers_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/identifier/bead.rs",
        "pub struct BeadId(String);\npub struct ParseBeadIdError { pub raw: String }\n",
    );
    let out = invoke(&["newtype_identifiers"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn newtype_identifiers_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/identifier/bead.rs",
        "pub struct BeadId { inner: String }\n",
    );
    let out = invoke(&["newtype_identifiers"], Some(ws.path()), None);
    assert_fail(&out, "BeadId");
}

// ---------------------------------------------------------------------------
// template_context_structs
// ---------------------------------------------------------------------------

#[test]
fn template_context_structs_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/run.md",
        "body\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "use askama::Template;\n\
         #[derive(Template)]\n\
         #[template(path = \"run.md\")]\n\
         pub struct RunContext;\n",
    );
    let out = invoke(&["template_context_structs"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn template_context_structs_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/run.md",
        "body\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub struct Nothing;\n",
    );
    let out = invoke(&["template_context_structs"], Some(ws.path()), None);
    assert_fail(&out, "run.md");
}

// ---------------------------------------------------------------------------
// no_hardcoded_tmp_paths (NFR #7)
// ---------------------------------------------------------------------------

// The forbidden prefix string is built at runtime via `concat!` so the
// fixture source itself doesn't carry the verbatim literal — the walk
// (and the legacy `loom/tests/style.rs`) self-scan would otherwise
// flag the fixture file.
const BANNED_PREFIX: &str = concat!("/", "tmp/");

#[test]
fn no_hardcoded_tmp_paths_pass() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/tests/foo.rs",
        "#[test]\nfn ok() {\n    let d = tempfile::tempdir().unwrap();\n    let _ = d.path();\n}\n",
    );
    let out = invoke(
        &["no_hardcoded_tmp_paths"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_hardcoded_tmp_paths_fail() {
    let ws = make_workspace();
    let body = format!(
        "#[test]\nfn bad() {{\n    let p = \"{BANNED_PREFIX}sneaky\";\n    let _ = p;\n}}\n"
    );
    let target = seed(ws.path(), "crates/loom-driver/tests/foo.rs", &body);
    let out = invoke(
        &["no_hardcoded_tmp_paths"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    let needle = format!("{BANNED_PREFIX}sneaky");
    assert_fail(&out, &needle);
}

// ---------------------------------------------------------------------------
// no_thread_sleep
// ---------------------------------------------------------------------------

#[test]
fn no_thread_sleep_pass() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub fn ok() { let _ = std::time::Duration::from_secs(1); }\n",
    );
    let out = invoke(
        &["no_thread_sleep"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_thread_sleep_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub fn bad() { std::thread::sleep(std::time::Duration::from_secs(1)); }\n",
    );
    let out = invoke(
        &["no_thread_sleep"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "thread::sleep");
}

// ---------------------------------------------------------------------------
// no_tokio_sleep_outside_clock
// ---------------------------------------------------------------------------

#[test]
fn no_tokio_sleep_outside_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub async fn sleep() { tokio::time::sleep(std::time::Duration::ZERO).await; }\n",
    );
    let out = invoke(
        &["no_tokio_sleep_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_tokio_sleep_outside_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub async fn bad() { tokio::time::sleep(std::time::Duration::ZERO).await; }\n",
    );
    let out = invoke(
        &["no_tokio_sleep_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "tokio::time::sleep");
}

// ---------------------------------------------------------------------------
// no_tokio_timeout_outside_clock
// ---------------------------------------------------------------------------

#[test]
fn no_tokio_timeout_outside_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub async fn timeout<F: std::future::Future>(f: F) { let _ = tokio::time::timeout(std::time::Duration::ZERO, f).await; }\n",
    );
    let out = invoke(
        &["no_tokio_timeout_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_tokio_timeout_outside_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub async fn bad() { let _ = tokio::time::timeout(std::time::Duration::ZERO, async {}).await; }\n",
    );
    let out = invoke(
        &["no_tokio_timeout_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "tokio::time::timeout");
}

// ---------------------------------------------------------------------------
// renderer_no_insta_dependency
// ---------------------------------------------------------------------------

const RENDERER_CARGO_OK: &str = "[package]\nname = \"loom-render\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";

#[test]
fn renderer_no_insta_dependency_pass_no_cargo_dep_no_rs_use() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    seed(
        ws.path(),
        "crates/loom-render/src/lib.rs",
        "pub fn render() -> String { String::new() }\n\
         #[cfg(test)]\n\
         mod tests {\n\
             #[test]\n\
             fn smoke() {\n\
                 assert!(super::render().is_empty());\n\
             }\n\
         }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn renderer_no_insta_dependency_fail_dev_dep_declared() {
    let ws = make_workspace();
    let cargo = "[package]\nname = \"loom-render\"\n\n[dev-dependencies]\ninsta = \"1\"\n";
    seed(ws.path(), "crates/loom-render/Cargo.toml", cargo);
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-render/Cargo.toml");
}

#[test]
fn renderer_no_insta_dependency_fail_use_in_test() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    seed(
        ws.path(),
        "crates/loom-render/src/renderer.rs",
        "#[cfg(test)]\n\
         mod tests {\n\
             use insta::assert_snapshot;\n\
             #[test]\n\
             fn snap() { assert_snapshot!(\"x\"); }\n\
         }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-render/src/renderer.rs");
}

#[test]
fn renderer_no_insta_dependency_ignores_other_crates() {
    let ws = make_workspace();
    // Renderer is clean.
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    // A different crate is allowed to use insta — only loom-render is in scope.
    seed(
        ws.path(),
        "crates/loom-templates/Cargo.toml",
        "[package]\nname = \"loom-templates\"\n\n[dev-dependencies]\ninsta = \"1\"\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/tests/snap.rs",
        "use insta::assert_snapshot;\n#[test] fn t() { assert_snapshot!(\"x\"); }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// no_real_clock_outside_system_clock
// ---------------------------------------------------------------------------

#[test]
fn no_real_clock_outside_system_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub fn now() -> std::time::Instant { std::time::Instant::now() }\n",
    );
    let out = invoke(
        &["no_real_clock_outside_system_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_real_clock_outside_system_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub fn bad() -> std::time::Instant { std::time::Instant::now() }\n",
    );
    let out = invoke(
        &["no_real_clock_outside_system_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "Instant::now");
}

// ---------------------------------------------------------------------------
// no_panics_in_production (RS-9)
// ---------------------------------------------------------------------------

#[test]
fn no_panics_in_production_pass_skips_cfg_test_blocks() {
    let ws = make_workspace();
    let body = format!(
        "pub fn ok() -> Result<u32, String> {{ Ok(0) }}\n\
         {cfg_test}\n\
         mod tests {{\n\
             #[test] fn t() {{ let _ = \"x\".{unwrap}(); }}\n\
         }}\n",
        cfg_test = concat!("#[", "cfg(test)]"),
        unwrap = "unwrap()",
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_panics_in_production_fail_unwrap_in_production() {
    let ws = make_workspace();
    let body = "pub fn bad() -> u32 { std::env::var(\"X\").unwrap().len() as u32 }\n";
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "unwrap");
}

#[test]
fn no_panics_in_production_pass_with_intermediate_attrs_on_cfg_test() {
    let ws = make_workspace();
    let body = format!(
        "{cfg_test}\n\
         {expect_attr}\n\
         mod tests {{\n\
             #[test] fn t() {{ {p}; }}\n\
         }}\n",
        cfg_test = concat!("#[", "cfg(test)]"),
        expect_attr = concat!("#[", "expect(clippy::expect_used, reason = \"tests\")]"),
        p = "panic!(\"x\")",
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// no_allow_dead_code (RS-10)
// ---------------------------------------------------------------------------

#[test]
fn no_allow_dead_code_pass_uses_expect() {
    let ws = make_workspace();
    let body = format!(
        "{expect_attr}\nstruct Unused;\n",
        expect_attr = concat!("#[", "expect(dead_code, reason = \"future use\")]"),
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_allow_dead_code"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_allow_dead_code_fail_uses_allow() {
    let ws = make_workspace();
    let body = format!(
        "{allow_attr}\nstruct Unused;\n",
        allow_attr = concat!("#[", "allow(dead_code)]"),
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_allow_dead_code"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "dead_code");
}

// ---------------------------------------------------------------------------
// loom_does_not_invoke_podman
// ---------------------------------------------------------------------------

#[test]
fn loom_does_not_invoke_podman_pass_doc_mention_ok() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-agent/src/pi/backend.rs",
        "//! drives the wrapix wrapper which talks to podman under the hood\npub fn ok() {}\n",
    );
    let out = invoke(
        &["loom_does_not_invoke_podman"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn loom_does_not_invoke_podman_fail_direct_command_new() {
    let ws = make_workspace();
    let body = format!(
        "pub fn bad() {{ let _ = {cmd_new}\"run\"); }}\n",
        cmd_new = concat!("Command::new(\"", "podman\").arg("),
    );
    let target = seed(ws.path(), "crates/loom-agent/src/pi/backend.rs", &body);
    let out = invoke(
        &["loom_does_not_invoke_podman"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "podman");
}

// ---------------------------------------------------------------------------
// crate_structure
// ---------------------------------------------------------------------------

const STRUCTURE_LIB_NAMES: &[&str] = &[
    "loom-events",
    "loom-driver",
    "loom-render",
    "loom-agent",
    "loom-workflow",
    "loom-templates",
];

fn seed_full_crate_set(ws: &TempDir) {
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nname=\"loom\"\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\n"),
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
}

#[test]
fn crate_structure_pass_all_seven_present() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let out = invoke(&["crate_structure"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn crate_structure_fail_missing_crate() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    // Wipe one library crate entirely.
    let _ = std::fs::remove_dir_all(ws.path().join("crates/loom-events"));
    let out = invoke(&["crate_structure"], Some(ws.path()), None);
    assert_fail(&out, "loom-events");
}

// ---------------------------------------------------------------------------
// workspace_edition
// ---------------------------------------------------------------------------

fn full_workspace_cargo() -> String {
    let mut s = String::from(
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom-driver\"]\n\n\
         [workspace.package]\nedition = \"2024\"\n",
    );
    s.push('\n');
    s
}

fn seed_full_workspace(ws: &TempDir) {
    std::fs::write(ws.path().join("Cargo.toml"), full_workspace_cargo()).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
}

#[test]
fn workspace_edition_pass_root_and_members() {
    let ws = make_workspace();
    seed_full_workspace(&ws);
    let out = invoke(&["workspace_edition"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_edition_fail_member_missing_inherit() {
    let ws = make_workspace();
    seed_full_workspace(&ws);
    // Replace one member's manifest with one that does NOT inherit edition.
    std::fs::write(
        ws.path().join("crates/loom-driver/Cargo.toml"),
        "[package]\nedition = \"2024\"\n[lints]\nworkspace = true\n",
    )
    .unwrap();
    let out = invoke(&["workspace_edition"], Some(ws.path()), None);
    assert_fail(&out, "edition.workspace");
}

// ---------------------------------------------------------------------------
// workspace_deps_pinned
// ---------------------------------------------------------------------------

#[test]
fn workspace_deps_pinned_pass_required_deps_present() {
    let ws = make_workspace();
    let mut cargo = String::from(
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom-driver\"]\n\n\
         [workspace.package]\nedition = \"2024\"\n\n[workspace.dependencies]\n",
    );
    for dep in [
        "tokio",
        "serde",
        "serde_json",
        "thiserror",
        "displaydoc",
        "anyhow",
        "tracing",
        "tracing-subscriber",
        "rusqlite",
        "toml",
        "askama",
        "clap",
        "gix",
        "fd-lock",
    ] {
        cargo.push_str(&format!("{dep} = \"1\"\n"));
    }
    std::fs::write(ws.path().join("Cargo.toml"), &cargo).unwrap();
    let out = invoke(&["workspace_deps_pinned"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_deps_pinned_fail_missing_required_dep() {
    let ws = make_workspace();
    // Default workspace Cargo has no [workspace.dependencies] section.
    let out = invoke(&["workspace_deps_pinned"], Some(ws.path()), None);
    assert_fail(&out, "[workspace.dependencies]");
}

// ---------------------------------------------------------------------------
// workspace_lints
// ---------------------------------------------------------------------------

#[test]
fn workspace_lints_pass_inheritance_present() {
    let ws = make_workspace();
    let cargo = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom\"]\n\n\
                 [workspace.package]\nedition = \"2024\"\n\n\
                 [workspace.lints.rust]\nunused = \"warn\"\n\n\
                 [workspace.lints.clippy]\npanic = \"deny\"\n";
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
    let out = invoke(&["workspace_lints"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_lints_fail_member_missing_workspace_true() {
    let ws = make_workspace();
    let cargo = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom\"]\n\n\
                 [workspace.package]\nedition = \"2024\"\n\n\
                 [workspace.lints.rust]\nunused = \"warn\"\n\n\
                 [workspace.lints.clippy]\npanic = \"deny\"\n";
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
    let out = invoke(&["workspace_lints"], Some(ws.path()), None);
    assert_fail(&out, "workspace = true");
}

// ---------------------------------------------------------------------------
// loom_events_minimal_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_events_minimal_deps_pass_exactly_three_runtime_deps() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\n",
    );
    let out = invoke(&["loom_events_minimal_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_events_minimal_deps_fail_extra_runtime_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\nchrono = \"0.4\"\n",
    );
    let out = invoke(&["loom_events_minimal_deps"], Some(ws.path()), None);
    assert_fail(&out, "chrono");
}

// ---------------------------------------------------------------------------
// loom_events_is_leaf
// ---------------------------------------------------------------------------

#[test]
fn loom_events_is_leaf_pass_no_internal_deps() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    let out = invoke(&["loom_events_is_leaf"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_events_is_leaf_fail_depends_on_loom_driver() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nloom-driver = { workspace = true }\n",
    );
    let out = invoke(&["loom_events_is_leaf"], Some(ws.path()), None);
    assert_fail(&out, "loom-driver");
}

// ---------------------------------------------------------------------------
// loom_render_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_render_deps_pass_depends_on_loom_events() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nloom-events = { workspace = true }\nserde_json = \"1\"\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_render_deps_fail_missing_loom_events() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nserde_json = \"1\"\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-events");
}

#[test]
fn loom_render_deps_fail_depends_on_loom_workflow() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nloom-events = { workspace = true }\nloom-workflow = { workspace = true }\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-workflow");
}

// ---------------------------------------------------------------------------
// phase_verdict_decide_called_from_production
// ---------------------------------------------------------------------------

#[test]
fn phase_verdict_decide_called_from_production_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/run/production.rs",
        "use crate::review::{decide};\npub fn run() { let _ = decide(&None, ()); }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/review/production.rs",
        "use super::phase_verdict::{decide};\npub fn review() { let _ = decide(&None, ()); }\n",
    );
    let out = invoke(
        &["phase_verdict_decide_called_from_production"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn phase_verdict_decide_called_from_production_fail_run_missing_call() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/run/production.rs",
        "pub fn run() { /* inlined classifier here, no decide call */ }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/review/production.rs",
        "use super::phase_verdict::{decide};\npub fn review() { let _ = decide(&None, ()); }\n",
    );
    let out = invoke(
        &["phase_verdict_decide_called_from_production"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "run/production.rs");
}

// ---------------------------------------------------------------------------
// no_sync_or_tune_command
// ---------------------------------------------------------------------------

#[test]
fn no_sync_or_tune_command_pass_when_absent() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom/src/main.rs",
        "fn main() {}\n\nenum Command {\n    Run,\n    Init,\n}\n",
    );
    let out = invoke(&["no_sync_or_tune_command"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn no_sync_or_tune_command_fail_when_sync_variant_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom/src/main.rs",
        "fn main() {}\n\nenum Command {\n    Run,\n    Sync,\n    Init,\n}\n",
    );
    let out = invoke(&["no_sync_or_tune_command"], Some(ws.path()), None);
    assert_fail(&out, "Sync");
}

#[test]
fn no_sync_or_tune_command_fail_when_tune_variant_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom/src/main.rs",
        "fn main() {}\n\nenum Command {\n    Run,\n    Tune(TuneArgs),\n}\n",
    );
    let out = invoke(&["no_sync_or_tune_command"], Some(ws.path()), None);
    assert_fail(&out, "Tune");
}

// ---------------------------------------------------------------------------
// template_pinning_matrix
// ---------------------------------------------------------------------------

fn seed_pinning_matrix(ws: &TempDir, matrix_body: &str) {
    seed(
        ws.path(),
        "specs/loom-templates.md",
        &format!(
            "# Loom Templates\n\n## Architecture\n\n### Pinning Policy\n\n{matrix_body}\n\n## Other\n"
        ),
    );
}

#[test]
fn template_pinning_matrix_pass_clean_matrix() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `run` |\n\
         |---|:-:|\n\
         | `context_pinning.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/run.md",
        "{% include \"partial/context_pinning.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/context_pinning.md",
        "ctx\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn template_pinning_matrix_fail_spec_marks_but_template_missing_include() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `run` |\n\
         |---|:-:|\n\
         | `style_rules.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/run.md",
        "no style_rules include here\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "style_rules.md");
}

#[test]
fn template_pinning_matrix_fail_template_includes_but_spec_blank() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `run` |\n\
         |---|:-:|\n\
         | `style_rules.md` |  |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/run.md",
        "{% include \"partial/style_rules.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/style_rules.md",
        "rules\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "marks the cell blank");
}

#[test]
fn template_pinning_matrix_resolves_transitive_includes() {
    let ws = make_workspace();
    // Spec marks `invariant_clash.md` ✓ for `plan_new`, and the template
    // pulls it in transitively via `plan_stage_rubric.md`.
    seed_pinning_matrix(
        &ws,
        "| Partial | `plan_new` |\n\
         |---|:-:|\n\
         | `plan_stage_rubric.md` | ✓ |\n\
         | `invariant_clash.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/plan_new.md",
        "{% include \"partial/plan_stage_rubric.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/plan_stage_rubric.md",
        "rubric\n{% include \"partial/invariant_clash.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/invariant_clash.md",
        "clash\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// loom_templates_snapshots_no_crate_root_allow
// ---------------------------------------------------------------------------

#[test]
fn loom_templates_snapshots_no_crate_root_allow_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/tests/snapshots.rs",
        "//! header doc.\n\nuse askama::Template;\n#[test] fn t() {}\n",
    );
    let out = invoke(
        &["loom_templates_snapshots_no_crate_root_allow"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_templates_snapshots_no_crate_root_allow_fail() {
    let ws = make_workspace();
    let body = format!(
        "{allow_attr}\nuse askama::Template;\n#[test] fn t() {{}}\n",
        allow_attr = concat!("#![", "allow(clippy::unwrap_used)]"),
    );
    seed(ws.path(), "crates/loom-templates/tests/snapshots.rs", &body);
    let out = invoke(
        &["loom_templates_snapshots_no_crate_root_allow"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crate-root `#![allow(...)]`");
}
