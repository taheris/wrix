//! Runner discovery for batched tiers (`[test]`, `[judge]`).
//!
//! Two layered mechanisms per `specs/loom-gate.md`: toolchain-detection
//! defaults (Cargo.toml → nextest, pyproject.toml → pytest, go.mod →
//! `go test`) and a `.loom/config.toml` override path for repos where the
//! defaults do not fit. The module also surfaces silent-zero-match cases
//! in cargo / nextest / pytest output so a filtered run that matches no
//! tests fails loudly rather than passing silently — other runners are
//! expected to fail on zero-match themselves and are passed through.

use std::fs;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;

use crate::annotation::Tier;

/// Template string for a batched-tier runner with a placeholder
/// substituted at invocation time. Default templates come from toolchain
/// detection; an opt-in `.loom/config.toml` overrides per tier.
///
/// Placeholder vocabulary, all rendered by [`RunnerTemplate::render`]:
///
/// - `{paths}` — slot-replicated and joined with ` | `. The slot is the
///   single-quoted phrase containing the placeholder (or the placeholder
///   token itself if no quotes wrap it). Matches `cargo nextest`'s
///   `-E 'test(p1) | test(p2)'` filter-expression shape.
/// - `{paths_or}` — replaced with the target list joined by ` or `
///   (pytest `-k` expression syntax).
/// - `{paths_alt}` — replaced with the target list joined by `|`
///   (regex alternation; `go test -run`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerTemplate {
    pub command: String,
}

impl RunnerTemplate {
    /// Construct from a raw template string.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }

    /// Substitute the placeholder for the joined target list and return
    /// the final command string ready to hand to a subprocess runner.
    pub fn render(&self, paths: &[&str]) -> String {
        render_template(&self.command, paths)
    }
}

/// Resolve the runner template for `tier` rooted at `repo_root`.
///
/// Order of resolution:
/// 1. `.loom/config.toml`'s `[runner]` table — per-tier override.
/// 2. Toolchain detection: `Cargo.toml` → nextest, `pyproject.toml` →
///    pytest, `go.mod` → `go test`.
/// 3. [`RunnerError::UnknownToolchain`] when neither path resolves.
///
/// Only batched tiers ([`Tier::Test`], [`Tier::Judge`]) are supported;
/// other tiers receive [`RunnerError::NotBatched`].
pub fn discover(repo_root: &Path, tier: Tier) -> Result<RunnerTemplate, RunnerError> {
    if !matches!(tier, Tier::Test | Tier::Judge) {
        return Err(RunnerError::NotBatched { tier });
    }

    if let Some(template) = load_override(repo_root, tier)? {
        return Ok(template);
    }

    if let Some(default) = detect_default(repo_root) {
        return Ok(default);
    }

    Err(RunnerError::UnknownToolchain {
        root: repo_root.to_path_buf(),
    })
}

/// Classification of a (template or rendered) command string for the
/// purposes of silent-zero-match sniffing.
///
/// `Unknown` covers any runner the gate does not know how to sniff —
/// the spec documents that those runners are responsible for failing on
/// zero-match themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    CargoTest,
    CargoNextest,
    Pytest,
    Unknown,
}

impl RunnerKind {
    /// Inspect the leading tokens of `command` and return the matching
    /// runner kind. The check is token-boundary aware, so a hypothetical
    /// `cargo testify` does not classify as `cargo test`.
    pub fn classify(command: &str) -> Self {
        let trimmed = command.trim_start();
        if starts_with_token(trimmed, "cargo nextest") {
            Self::CargoNextest
        } else if starts_with_token(trimmed, "cargo test") {
            Self::CargoTest
        } else if starts_with_token(trimmed, "pytest") {
            Self::Pytest
        } else {
            Self::Unknown
        }
    }

    /// Human-readable name embedded in zero-match diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::CargoTest => "cargo test",
            Self::CargoNextest => "cargo nextest",
            Self::Pytest => "pytest",
            Self::Unknown => "unknown",
        }
    }
}

/// Post-process the runner's stdout / stderr after a successful exit and
/// surface [`RunnerError::ZeroMatch`] when the run silently matched
/// nothing.
///
/// Returns `Ok(())` for [`RunnerKind::Unknown`] — per the spec, the gate
/// documents the fail-on-zero-match expectation for unrecognised runners
/// but does not enforce it.
pub fn check_zero_match(command: &str, stdout: &str, stderr: &str) -> Result<(), RunnerError> {
    let kind = RunnerKind::classify(command);
    if let Some(evidence) = detect_zero_match(kind, stdout, stderr) {
        return Err(RunnerError::ZeroMatch {
            runner: kind.name(),
            evidence,
        });
    }
    Ok(())
}

fn detect_zero_match(kind: RunnerKind, stdout: &str, stderr: &str) -> Option<String> {
    match kind {
        RunnerKind::CargoTest => stdout
            .lines()
            .find(|l| l.trim() == "running 0 tests")
            .map(|l| l.trim().to_string()),
        RunnerKind::CargoNextest => stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .find(|t| {
                t.starts_with("Starting 0 tests")
                    || t.contains("0 tests run:")
                    || t.contains("no tests to run")
            })
            .map(str::to_string),
        RunnerKind::Pytest => stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .find(|t| t.contains("no tests ran") || t.contains("collected 0 items"))
            .map(str::to_string),
        RunnerKind::Unknown => None,
    }
}

fn load_override(repo_root: &Path, tier: Tier) -> Result<Option<RunnerTemplate>, RunnerError> {
    let path = repo_root.join(".loom").join("config.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let body = fs::read_to_string(&path).map_err(|e| RunnerError::ReadConfig {
        path: path.clone(),
        source: e,
    })?;
    let cfg: LoomConfig = toml::from_str(&body).map_err(|e| RunnerError::ParseConfig {
        path: path.clone(),
        source: e,
    })?;
    let entry = cfg.runner.and_then(|r| match tier {
        Tier::Test => r.test,
        Tier::Judge => r.judge,
        Tier::Check | Tier::System => None,
    });
    Ok(entry.map(RunnerTemplate::new))
}

fn detect_default(repo_root: &Path) -> Option<RunnerTemplate> {
    if repo_root.join("Cargo.toml").is_file() {
        return Some(RunnerTemplate::new("cargo nextest run -E 'test({paths})'"));
    }
    if repo_root.join("pyproject.toml").is_file() {
        return Some(RunnerTemplate::new("pytest -k '{paths_or}'"));
    }
    if repo_root.join("go.mod").is_file() {
        return Some(RunnerTemplate::new("go test -run '{paths_alt}' ./..."));
    }
    None
}

#[derive(Debug, Deserialize)]
struct LoomConfig {
    runner: Option<RunnerSection>,
}

#[derive(Debug, Deserialize)]
struct RunnerSection {
    test: Option<String>,
    judge: Option<String>,
}

fn render_template(template: &str, paths: &[&str]) -> String {
    let mut s = template.to_string();
    if s.contains("{paths_or}") {
        s = s.replace("{paths_or}", &paths.join(" or "));
    }
    if s.contains("{paths_alt}") {
        s = s.replace("{paths_alt}", &paths.join("|"));
    }
    if let Some(start) = s.find("{paths}") {
        s = render_slot(&s, start, paths);
    }
    s
}

/// Replicate the slot surrounding `{paths}` per target and join with
/// ` | `. The slot is bounded by the nearest `'` on either side, or by
/// the start / end of the template when no quote is present. This makes
/// `cargo nextest run -E 'test({paths})'` expand to
/// `cargo nextest run -E 'test(p1) | test(p2)'`.
fn render_slot(template: &str, start: usize, paths: &[&str]) -> String {
    const PLACEHOLDER: &str = "{paths}";
    let end = start + PLACEHOLDER.len();

    let before = &template[..start];
    let after = &template[end..];

    let slot_lstart = before.rfind('\'').map_or(0, |i| i + 1);
    let slot_rend_local = after.find('\'').unwrap_or(after.len());

    let slot_prefix = &before[slot_lstart..];
    let slot_suffix = &after[..slot_rend_local];

    let joined = paths
        .iter()
        .map(|p| format!("{slot_prefix}{p}{slot_suffix}"))
        .collect::<Vec<_>>()
        .join(" | ");

    let before_slot = &template[..slot_lstart];
    let after_slot = &template[end + slot_rend_local..];

    format!("{before_slot}{joined}{after_slot}")
}

fn starts_with_token(input: &str, token: &str) -> bool {
    input
        .strip_prefix(token)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// Failures surfaced by runner discovery and zero-match sniffing.
#[derive(Debug, Display, Error)]
pub enum RunnerError {
    /// runner discovery only applies to batched tiers (test, judge); got [{tier}]
    NotBatched { tier: Tier },
    /// no .loom/config.toml override and no Cargo.toml / pyproject.toml / go.mod under {root}
    UnknownToolchain { root: PathBuf },
    /// failed to read runner override at {path}: {source}
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to parse runner override at {path}: {source}
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// {runner} reported zero matched tests; filter likely missed every target: {evidence}
    ZeroMatch {
        runner: &'static str,
        evidence: String,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn detect_default_for_cargo_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "cargo nextest run -E 'test({paths})'");

        let judge = discover(dir.path(), Tier::Judge).unwrap();
        assert_eq!(judge.command, "cargo nextest run -E 'test({paths})'");
    }

    #[test]
    fn detect_default_for_python_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "pytest -k '{paths_or}'");
    }

    #[test]
    fn detect_default_for_go_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/x\n").unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "go test -run '{paths_alt}' ./...");
    }

    #[test]
    fn override_test_via_loom_config_takes_precedence() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let loom = dir.path().join(".loom");
        fs::create_dir_all(&loom).unwrap();
        fs::write(
            loom.join("config.toml"),
            "[runner]\ntest = \"my-runner --tests {paths}\"\n",
        )
        .unwrap();

        let template = discover(dir.path(), Tier::Test).unwrap();
        assert_eq!(template.command, "my-runner --tests {paths}");
    }

    #[test]
    fn override_judge_via_loom_config() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let loom = dir.path().join(".loom");
        fs::create_dir_all(&loom).unwrap();
        fs::write(
            loom.join("config.toml"),
            "[runner]\njudge = \"loom-judge {paths}\"\n",
        )
        .unwrap();

        let judge = discover(dir.path(), Tier::Judge).unwrap();
        assert_eq!(judge.command, "loom-judge {paths}");
    }

    #[test]
    fn override_falls_back_per_tier_when_entry_missing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let loom = dir.path().join(".loom");
        fs::create_dir_all(&loom).unwrap();
        fs::write(
            loom.join("config.toml"),
            "[runner]\ntest = \"my-runner {paths}\"\n",
        )
        .unwrap();

        let judge = discover(dir.path(), Tier::Judge).unwrap();
        assert_eq!(
            judge.command, "cargo nextest run -E 'test({paths})'",
            "judge entry missing in override → falls back to toolchain default"
        );
    }

    #[test]
    fn unknown_toolchain_errors_cleanly() {
        let dir = tempdir().unwrap();
        let err = discover(dir.path(), Tier::Test).unwrap_err();
        assert!(matches!(err, RunnerError::UnknownToolchain { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains("no .loom/config.toml override"),
            "message names the override path: {msg}"
        );
        assert!(
            msg.contains("Cargo.toml") && msg.contains("pyproject.toml") && msg.contains("go.mod"),
            "message names the detected markers: {msg}"
        );
    }

    #[test]
    fn check_and_system_tiers_are_not_batched() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let check = discover(dir.path(), Tier::Check).unwrap_err();
        assert!(matches!(
            check,
            RunnerError::NotBatched { tier: Tier::Check }
        ));
        let system = discover(dir.path(), Tier::System).unwrap_err();
        assert!(matches!(
            system,
            RunnerError::NotBatched { tier: Tier::System }
        ));
    }

    #[test]
    fn parse_error_surfaces_with_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let loom = dir.path().join(".loom");
        fs::create_dir_all(&loom).unwrap();
        fs::write(loom.join("config.toml"), "this is = not valid = toml\n").unwrap();

        let err = discover(dir.path(), Tier::Test).unwrap_err();
        match err {
            RunnerError::ParseConfig { path, .. } => {
                assert!(path.ends_with(".loom/config.toml"), "{path:?}");
            }
            other => panic!("expected ParseConfig, got {other:?}"),
        }
    }

    #[test]
    fn render_substitutes_paths_or_with_or_join() {
        let t = RunnerTemplate::new("pytest -k '{paths_or}'");
        assert_eq!(t.render(&["a", "b", "c"]), "pytest -k 'a or b or c'");
    }

    #[test]
    fn render_substitutes_paths_alt_with_pipe_join() {
        let t = RunnerTemplate::new("go test -run '{paths_alt}' ./...");
        assert_eq!(
            t.render(&["TestA", "TestB"]),
            "go test -run 'TestA|TestB' ./..."
        );
    }

    #[test]
    fn render_slot_replicates_within_single_quotes_for_nextest() {
        let t = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        assert_eq!(
            t.render(&["p1", "p2", "p3"]),
            "cargo nextest run -E 'test(p1) | test(p2) | test(p3)'"
        );
    }

    #[test]
    fn render_slot_with_single_target_emits_single_slot() {
        let t = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        assert_eq!(t.render(&["solo"]), "cargo nextest run -E 'test(solo)'");
    }

    #[test]
    fn render_slot_without_quotes_uses_whole_template_as_slot() {
        let t = RunnerTemplate::new("mytool {paths}");
        assert_eq!(t.render(&["a", "b"]), "mytool a | mytool b");
    }

    #[test]
    fn render_passes_through_template_with_no_placeholder() {
        let t = RunnerTemplate::new("no-placeholder-here");
        assert_eq!(t.render(&["a"]), "no-placeholder-here");
    }

    #[test]
    fn render_full_toolchain_defaults_round_trip() {
        let cargo = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let pytest = RunnerTemplate::new("pytest -k '{paths_or}'");
        let go = RunnerTemplate::new("go test -run '{paths_alt}' ./...");

        assert_eq!(
            cargo.render(&["mod::a", "mod::b"]),
            "cargo nextest run -E 'test(mod::a) | test(mod::b)'"
        );
        assert_eq!(
            pytest.render(&["test_a", "test_b"]),
            "pytest -k 'test_a or test_b'"
        );
        assert_eq!(
            go.render(&["TestA", "TestB"]),
            "go test -run 'TestA|TestB' ./..."
        );
    }

    #[test]
    fn classify_recognises_cargo_test_cargo_nextest_pytest() {
        assert_eq!(
            RunnerKind::classify("cargo test --workspace"),
            RunnerKind::CargoTest
        );
        assert_eq!(
            RunnerKind::classify("cargo nextest run -E 'test(x)'"),
            RunnerKind::CargoNextest
        );
        assert_eq!(RunnerKind::classify("pytest -k x"), RunnerKind::Pytest);
        assert_eq!(RunnerKind::classify("my-runner"), RunnerKind::Unknown);
        assert_eq!(RunnerKind::classify(""), RunnerKind::Unknown);
    }

    #[test]
    fn classify_token_boundary_avoids_false_positive_on_cargo_testify() {
        assert_eq!(
            RunnerKind::classify("cargo testify --foo"),
            RunnerKind::Unknown
        );
        assert_eq!(
            RunnerKind::classify("pytest-something"),
            RunnerKind::Unknown
        );
    }

    #[test]
    fn zero_match_detects_cargo_test_running_zero_tests() {
        let stdout = "\
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out
";
        let err = check_zero_match("cargo test -- missing_name", stdout, "").unwrap_err();
        match err {
            RunnerError::ZeroMatch { runner, evidence } => {
                assert_eq!(runner, "cargo test");
                assert_eq!(evidence, "running 0 tests");
            }
            other => panic!("expected ZeroMatch, got {other:?}"),
        }
    }

    #[test]
    fn zero_match_detects_cargo_nextest_starting_zero_tests() {
        let stdout =
            "    Starting 0 tests across 5 binaries (run ID: abc, nextest profile: default)\n";
        let err = check_zero_match("cargo nextest run -E 'test(nope)'", stdout, "").unwrap_err();
        assert!(matches!(
            err,
            RunnerError::ZeroMatch {
                runner: "cargo nextest",
                ..
            }
        ));
    }

    #[test]
    fn zero_match_detects_cargo_nextest_summary_zero_tests_run() {
        let stdout = "------------\n     Summary [   0.011s] 0 tests run: 0 passed, 0 skipped\n";
        let err = check_zero_match("cargo nextest run", stdout, "").unwrap_err();
        assert!(matches!(err, RunnerError::ZeroMatch { .. }));
    }

    #[test]
    fn zero_match_detects_pytest_no_tests_ran() {
        let stdout =
            "collected 0 items\n\n================= no tests ran in 0.01s =================\n";
        let err = check_zero_match("pytest -k missing", stdout, "").unwrap_err();
        assert!(matches!(
            err,
            RunnerError::ZeroMatch {
                runner: "pytest",
                ..
            }
        ));
    }

    #[test]
    fn zero_match_passes_when_runner_actually_ran_tests() {
        let stdout = "\
running 3 tests
test alpha ... ok
test beta ... ok
test gamma ... ok

test result: ok. 3 passed; 0 failed
";
        check_zero_match("cargo test", stdout, "").unwrap();
    }

    #[test]
    fn zero_match_passes_for_unrecognised_runner_even_with_zero_in_output() {
        let stdout = "running 0 tests\n";
        check_zero_match("my-custom-runner --tests x", stdout, "").unwrap();
    }

    #[test]
    fn zero_match_inspects_stderr_for_pytest_and_nextest() {
        let stderr = "Starting 0 tests across 1 binaries (run ID: abc, nextest profile: default)";
        let err = check_zero_match("cargo nextest run", "", stderr).unwrap_err();
        assert!(matches!(err, RunnerError::ZeroMatch { .. }));
    }

    #[test]
    fn not_batched_error_message_names_the_tier() {
        let err = RunnerError::NotBatched { tier: Tier::Check };
        assert_eq!(
            err.to_string(),
            "runner discovery only applies to batched tiers (test, judge); got [check]"
        );
    }
}
