//! Per-tier dispatcher.
//!
//! Routes each [`Annotation`] to its verifier per the verifier-runner
//! contract in `specs/loom-gate.md`. `[check]` and `[system]` annotations
//! each spawn one subprocess; `[test]` annotations collect into a single
//! batched runner invocation (filtered against `--files` scope via the
//! [`TestScope`] trait); `[judge]` annotations collect into a single
//! batched runner invocation (no scope filter — judges are LLM-driven
//! and don't reduce to file ownership).
//!
//! Each verifier subprocess receives `LOOM_FILES` (colon-joined paths)
//! and `LOOM_SPEC` (when set) on its environment and is expected to
//! emit one `{"pass": bool, "evidence": "<msg>"}` JSON line on stdout
//! with an exit code mirroring `pass`. For batched runners that do not
//! conform to the JSON-line contract (e.g. raw `cargo nextest`), the
//! dispatcher falls back to exit-code interpretation with the runner's
//! stderr surfaced as evidence.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Output};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::annotation::{Annotation, Tier};
use crate::runner::{RunnerError, RunnerTemplate, check_zero_match};

/// JSON-line verdict every verifier returns on stdout, per the
/// verifier-runner contract in `specs/loom-gate.md`. The exit code mirrors
/// `pass` (0 for true, non-zero for false); the gate parses one line of
/// JSON-encoded `VerifierVerdict` from each verifier's stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierVerdict {
    pub pass: bool,
    pub evidence: String,
}

/// Failures the dispatcher surfaces. Per RS-4 each variant carries the
/// command string and original error so callers can route the error back
/// to a specific annotation source line.
#[derive(Debug, Display, Error)]
pub enum DispatchError {
    /// annotation target was empty for tier [{tier}]
    EmptyTarget { tier: Tier },
    /// failed to spawn verifier `{command}`: {source}
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// verifier `{command}` produced no JSON verdict line on stdout
    NoVerdictLine { command: String },
    /// verifier `{command}` produced malformed JSON verdict: {source}
    MalformedVerdict {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    /// batched runner zero-match: {source}
    ZeroMatch {
        #[source]
        source: RunnerError,
    },
}

/// Options shared by every dispatch entry point: the `--files` scope set
/// (colon-joined into `LOOM_FILES`) and the optional `--spec` label
/// (forwarded as `LOOM_SPEC`). An empty `files` vec means "no `--files`
/// filter" — verifiers see an empty `LOOM_FILES` and batched tiers skip
/// scope intersection.
#[derive(Debug, Default, Clone)]
pub struct DispatchOptions {
    pub files: Vec<PathBuf>,
    pub spec: Option<String>,
}

/// Result of dispatching one verifier — either a single annotation
/// ([`run_check`] / [`run_system`]) or a batch of annotations sharing one
/// subprocess ([`run_test`] / [`run_judge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub annotations: Vec<Annotation>,
    pub verdict: VerifierVerdict,
}

/// Map a `[test]` annotation to its source-file scope so the dispatcher
/// can intersect against `--files` before issuing the batched runner.
///
/// Production implementations consult cargo metadata to walk the
/// transitive dependency graph for the annotation's owning crate; tests
/// substitute a deterministic stub. See [`EmptyScope`] for the
/// no-filtering default.
pub trait TestScope {
    /// Source files inside the annotation's scope (its owning crate plus
    /// transitive deps for Rust workspaces; toolchain-specific analogues
    /// elsewhere).
    fn scope_for(&self, annotation: &Annotation) -> Vec<PathBuf>;
}

/// Scope impl that reports the empty set for every annotation. With
/// `--files` empty (no filter requested) the dispatcher skips
/// intersection and every annotation passes through; with `--files`
/// set, every annotation is filtered out. This is the safe default
/// before a cargo-metadata-backed scope lands.
pub struct EmptyScope;

impl TestScope for EmptyScope {
    fn scope_for(&self, _annotation: &Annotation) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Dispatch every `[check]`-tier annotation in `annotations`. One
/// subprocess per annotation, no batching. Returns one result per
/// annotation in input order — a `Result` per entry so a single
/// misbehaving verifier doesn't sink the rest of the batch.
pub fn run_check(
    annotations: &[Annotation],
    options: &DispatchOptions,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    run_per_annotation(annotations, Tier::Check, options)
}

/// Dispatch every `[system]`-tier annotation in `annotations`. One
/// subprocess per annotation, identical shape to [`run_check`].
pub fn run_system(
    annotations: &[Annotation],
    options: &DispatchOptions,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    run_per_annotation(annotations, Tier::System, options)
}

/// Dispatch every `[test]`-tier annotation in `annotations` as one
/// batched runner subprocess. Targets are filtered by `--files` scope
/// via the [`TestScope`] resolver before being passed to the runner
/// template; an empty filter result returns `Ok(None)` so the caller
/// can distinguish "skipped — no scope match" from a true verdict.
pub fn run_test(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
    scope: &dyn TestScope,
) -> Result<Option<DispatchOutcome>, DispatchError> {
    let candidates: Vec<&Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::Test)
        .collect();
    let filtered = filter_by_files(&candidates, &options.files, scope);
    if filtered.is_empty() {
        return Ok(None);
    }
    let targets: Vec<&str> = filtered.iter().map(|a| a.target.as_str()).collect();
    let command = template.render(&targets);
    let verdict = run_batched(&command, options, true)?;
    Ok(Some(DispatchOutcome {
        annotations: filtered.into_iter().cloned().collect(),
        verdict,
    }))
}

/// Dispatch every `[judge]`-tier annotation in `annotations` as one
/// batched runner subprocess. Judges aren't `--files`-filterable, so
/// every judge annotation is included.
pub fn run_judge(
    annotations: &[Annotation],
    options: &DispatchOptions,
    template: &RunnerTemplate,
) -> Result<Option<DispatchOutcome>, DispatchError> {
    let judges: Vec<&Annotation> = annotations
        .iter()
        .filter(|a| a.tier == Tier::Judge)
        .collect();
    if judges.is_empty() {
        return Ok(None);
    }
    let targets: Vec<&str> = judges.iter().map(|a| a.target.as_str()).collect();
    let command = template.render(&targets);
    let verdict = run_batched(&command, options, false)?;
    Ok(Some(DispatchOutcome {
        annotations: judges.into_iter().cloned().collect(),
        verdict,
    }))
}

fn run_per_annotation(
    annotations: &[Annotation],
    tier: Tier,
    options: &DispatchOptions,
) -> Vec<Result<DispatchOutcome, DispatchError>> {
    annotations
        .iter()
        .filter(|a| a.tier == tier)
        .map(|a| run_single(a, options))
        .collect()
}

fn run_single(
    annotation: &Annotation,
    options: &DispatchOptions,
) -> Result<DispatchOutcome, DispatchError> {
    let command = annotation.target.trim();
    if command.is_empty() {
        return Err(DispatchError::EmptyTarget {
            tier: annotation.tier,
        });
    }
    let output = spawn(command, options)?;
    let verdict = parse_verdict_required(command, &output)?;
    Ok(DispatchOutcome {
        annotations: vec![annotation.clone()],
        verdict,
    })
}

fn run_batched(
    command: &str,
    options: &DispatchOptions,
    sniff_zero_match: bool,
) -> Result<VerifierVerdict, DispatchError> {
    let output = spawn(command, options)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if sniff_zero_match {
        check_zero_match(command, &stdout, &stderr)
            .map_err(|e| DispatchError::ZeroMatch { source: e })?;
    }
    if let Some(verdict) = parse_verdict_optional(command, &stdout)? {
        return Ok(verdict);
    }
    Ok(VerifierVerdict {
        pass: output.status.success(),
        evidence: if output.status.success() {
            stdout.into_owned()
        } else {
            stderr.into_owned()
        },
    })
}

fn filter_by_files<'a>(
    candidates: &[&'a Annotation],
    files: &[PathBuf],
    scope: &dyn TestScope,
) -> Vec<&'a Annotation> {
    if files.is_empty() {
        return candidates.to_vec();
    }
    let file_set: HashSet<&PathBuf> = files.iter().collect();
    candidates
        .iter()
        .copied()
        .filter(|a| {
            scope
                .scope_for(a)
                .iter()
                .any(|path| file_set.contains(path))
        })
        .collect()
}

fn spawn(command: &str, options: &DispatchOptions) -> Result<Output, DispatchError> {
    let mut tokens = command.split_whitespace();
    let head = tokens.next().ok_or_else(|| DispatchError::Spawn {
        command: command.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"),
    })?;
    let tail: Vec<&str> = tokens.collect();
    let mut cmd = Command::new(head);
    cmd.args(&tail);
    cmd.env("LOOM_FILES", encode_files(&options.files));
    if let Some(spec) = &options.spec {
        cmd.env("LOOM_SPEC", spec);
    }
    cmd.output().map_err(|e| DispatchError::Spawn {
        command: command.to_string(),
        source: e,
    })
}

fn encode_files(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

fn parse_verdict_required(
    command: &str,
    output: &Output,
) -> Result<VerifierVerdict, DispatchError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_verdict_optional(command, &stdout)?.ok_or_else(|| DispatchError::NoVerdictLine {
        command: command.to_string(),
    })
}

fn parse_verdict_optional(
    command: &str,
    stdout: &str,
) -> Result<Option<VerifierVerdict>, DispatchError> {
    for raw in stdout.lines().rev() {
        let line = raw.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        return serde_json::from_str::<VerifierVerdict>(line)
            .map(Some)
            .map_err(|source| DispatchError::MalformedVerdict {
                command: command.to_string(),
                source,
            });
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ann(tier: Tier, target: &str) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from("specs/a.md"),
            line: 1,
            criterion_line: 1,
        }
    }

    #[test]
    fn verdict_round_trips_through_json() {
        let v = VerifierVerdict {
            pass: true,
            evidence: "ok".into(),
        };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"pass":true,"evidence":"ok"}"#);
        let back: VerifierVerdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn parse_verdict_optional_picks_last_json_line() {
        let stdout = "warning: deprecation\nfoo bar\n{\"pass\": true, \"evidence\": \"ok\"}\n";
        let v = parse_verdict_optional("cmd", stdout).unwrap().unwrap();
        assert!(v.pass);
        assert_eq!(v.evidence, "ok");
    }

    #[test]
    fn parse_verdict_optional_returns_none_when_no_json() {
        let stdout = "no JSON here\nrunning some tests\nall good\n";
        assert!(parse_verdict_optional("cmd", stdout).unwrap().is_none());
    }

    #[test]
    fn parse_verdict_required_errors_when_missing() {
        let output = Output {
            status: std::process::ExitStatus::default(),
            stdout: b"no json here\n".to_vec(),
            stderr: Vec::new(),
        };
        let err = parse_verdict_required("the-cmd", &output).unwrap_err();
        match err {
            DispatchError::NoVerdictLine { command } => assert_eq!(command, "the-cmd"),
            other => panic!("expected NoVerdictLine, got {other:?}"),
        }
    }

    #[test]
    fn parse_verdict_optional_surfaces_malformed_json_error() {
        let stdout = "{\"pass\": maybe}\n";
        let err = parse_verdict_optional("cmd", stdout).unwrap_err();
        assert!(matches!(err, DispatchError::MalformedVerdict { .. }));
    }

    #[test]
    fn empty_scope_returns_empty_for_every_annotation() {
        let scope = EmptyScope;
        let a = ann(Tier::Test, "crate::a::ok");
        assert!(scope.scope_for(&a).is_empty());
    }

    struct StubScope(std::collections::HashMap<String, Vec<PathBuf>>);

    impl TestScope for StubScope {
        fn scope_for(&self, a: &Annotation) -> Vec<PathBuf> {
            self.0.get(&a.target).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn filter_by_files_keeps_intersecting_annotations() {
        let a = ann(Tier::Test, "crate::a::keep");
        let b = ann(Tier::Test, "crate::b::drop");
        let candidates = vec![&a, &b];
        let scope = StubScope(
            [
                (
                    "crate::a::keep".to_string(),
                    vec![PathBuf::from("src/a.rs")],
                ),
                (
                    "crate::b::drop".to_string(),
                    vec![PathBuf::from("src/b.rs")],
                ),
            ]
            .into_iter()
            .collect(),
        );
        let files = vec![PathBuf::from("src/a.rs")];
        let kept = filter_by_files(&candidates, &files, &scope);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "crate::a::keep");
    }

    #[test]
    fn filter_by_files_with_empty_filter_passes_everything_through() {
        let a = ann(Tier::Test, "x");
        let b = ann(Tier::Test, "y");
        let candidates = vec![&a, &b];
        let scope = EmptyScope;
        let kept = filter_by_files(&candidates, &[], &scope);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn filter_by_files_with_empty_scope_drops_everything_when_filter_set() {
        let a = ann(Tier::Test, "x");
        let candidates = vec![&a];
        let scope = EmptyScope;
        let files = vec![PathBuf::from("src/a.rs")];
        let kept = filter_by_files(&candidates, &files, &scope);
        assert!(kept.is_empty());
    }

    #[test]
    fn encode_files_joins_with_colon() {
        let files = vec![
            PathBuf::from("a/b.rs"),
            PathBuf::from("c.rs"),
            PathBuf::from("d/e/f.rs"),
        ];
        assert_eq!(encode_files(&files), "a/b.rs:c.rs:d/e/f.rs");
    }

    #[test]
    fn encode_files_empty_yields_empty_string() {
        assert_eq!(encode_files(&[]), "");
    }

    #[test]
    fn dispatch_options_default_is_unfiltered() {
        let opts = DispatchOptions::default();
        assert!(opts.files.is_empty());
        assert!(opts.spec.is_none());
    }

    #[test]
    fn empty_target_error_message_names_the_tier() {
        let e = DispatchError::EmptyTarget { tier: Tier::Check };
        assert_eq!(
            e.to_string(),
            "annotation target was empty for tier [check]"
        );
    }

    #[test]
    fn no_verdict_line_error_message_names_the_command() {
        let e = DispatchError::NoVerdictLine {
            command: "my-walk x".into(),
        };
        assert_eq!(
            e.to_string(),
            "verifier `my-walk x` produced no JSON verdict line on stdout"
        );
    }

    #[test]
    fn run_check_returns_one_result_per_check_annotation() {
        let inputs = vec![
            ann(Tier::Check, "/no/such/script-1"),
            ann(Tier::System, "/no/such/system"),
            ann(Tier::Check, "/no/such/script-2"),
        ];
        let opts = DispatchOptions::default();
        let results = run_check(&inputs, &opts);
        assert_eq!(results.len(), 2, "filters to Check tier only");
        for r in &results {
            assert!(matches!(r, Err(DispatchError::Spawn { .. })));
        }
    }

    #[test]
    fn run_system_returns_one_result_per_system_annotation() {
        let inputs = vec![
            ann(Tier::System, "/no/such/system-1"),
            ann(Tier::Check, "/no/such/check"),
            ann(Tier::System, "/no/such/system-2"),
        ];
        let opts = DispatchOptions::default();
        let results = run_system(&inputs, &opts);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn run_test_returns_none_when_no_test_annotations_present() {
        let inputs = vec![ann(Tier::Check, "x")];
        let template = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let opts = DispatchOptions::default();
        let result = run_test(&inputs, &opts, &template, &EmptyScope).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn run_test_returns_none_when_scope_filter_excludes_every_annotation() {
        let inputs = vec![ann(Tier::Test, "crate::a::ok")];
        let template = RunnerTemplate::new("cargo nextest run -E 'test({paths})'");
        let opts = DispatchOptions {
            files: vec![PathBuf::from("src/a.rs")],
            spec: None,
        };
        let result = run_test(&inputs, &opts, &template, &EmptyScope).unwrap();
        assert!(
            result.is_none(),
            "EmptyScope intersected against non-empty files filter excludes all"
        );
    }

    #[test]
    fn run_judge_returns_none_when_no_judge_annotations_present() {
        let inputs = vec![ann(Tier::Test, "crate::a::ok")];
        let template = RunnerTemplate::new("loom-judge {paths}");
        let opts = DispatchOptions::default();
        let result = run_judge(&inputs, &opts, &template).unwrap();
        assert!(result.is_none());
    }

    /// Lightweight subprocess assertion: verify [`run_check`] invokes
    /// the annotation target with the env contract and parses the JSON
    /// verdict line. Targets `sh <script>` rather than executing the
    /// script directly so the test doesn't depend on a chmod race with
    /// the kernel's `ETXTBSY` guard on freshly-written executables.
    #[test]
    fn run_check_spawns_subprocess_and_parses_verdict_from_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("verifier.sh");
        std::fs::write(
            &script,
            "printf '{\"pass\": true, \"evidence\": \"hello\"}\\n'\n",
        )
        .unwrap();

        let target = format!("sh {}", script.display());
        let inputs = vec![ann(Tier::Check, &target)];
        let opts = DispatchOptions::default();
        let results = run_check(&inputs, &opts);
        assert_eq!(results.len(), 1);
        let outcome = results.into_iter().next().unwrap().unwrap();
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.evidence, "hello");
    }

    #[test]
    fn run_check_propagates_env_loom_files_and_loom_spec_to_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("envcheck.sh");
        std::fs::write(
            &script,
            "printf '{\"pass\": true, \"evidence\": \"FILES=%s SPEC=%s\"}\\n' \"$LOOM_FILES\" \"$LOOM_SPEC\"\n",
        )
        .unwrap();

        let target = format!("sh {}", script.display());
        let inputs = vec![ann(Tier::Check, &target)];
        let opts = DispatchOptions {
            files: vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            spec: Some("loom-tests".into()),
        };
        let results = run_check(&inputs, &opts);
        let outcome = results.into_iter().next().unwrap().unwrap();
        assert_eq!(outcome.verdict.evidence, "FILES=a.rs:b.rs SPEC=loom-tests");
    }
}
