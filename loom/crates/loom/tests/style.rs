//! AST + filesystem style enforcement that clippy can't express.
//!
//! Currently asserts: the run-time renderer's test files do NOT depend on
//! `insta`. The renderer is a flexibility surface (terminal tool-call lines,
//! status colors, truncation) — substring + structural assertions are the
//! contract per `specs/loom-tests.md` *Snapshot Testing*. A snapshot here
//! would lock down layout decisions the spec deliberately leaves free.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Files that exercise the run-time renderer (`TerminalRenderer` /
/// `LogSink`). New renderer test files MUST be added here so the no-insta
/// assertion keeps applying.
const RENDERER_TEST_FILES: &[&str] = &["crates/loom-core/tests/logging.rs"];

fn loom_workspace_root() -> PathBuf {
    // `tests/style.rs` runs from `loom/crates/loom`. The workspace root is
    // two ancestors up.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("workspace root above crates/loom")
}

#[test]
fn renderer_no_insta_dependency() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for rel in RENDERER_TEST_FILES {
        let path = root.join(rel);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (lineno, line) in body.lines().enumerate() {
            // Skip comments — discussing the rule itself shouldn't trip it.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            if line.contains("insta::") || line.contains("use insta") {
                violations.push(format!("{}:{}: {}", rel, lineno + 1, line));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "renderer test files must not depend on `insta` (use substring + \
         structural assertions instead — see specs/loom-tests.md \
         §Snapshot Testing). Violations:\n{}",
        violations.join("\n"),
    );
}
