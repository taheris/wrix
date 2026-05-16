//! `loom check --check=criteria` — spec ↔ test verifier audit (FR14).
//!
//! Walks every `specs/*.md` for `[verify](tests/loom-test.sh::test_<fn>)`
//! annotations and produces a per-criterion verdict by running the
//! annotated dispatcher live against the current code-spec pair. Per
//! FR14 the spec markdown carries no `[ ]` / `[x]` checkbox — status is
//! a property of running the verifier, not a value stored in the spec.
//!
//! ## Per-criterion verdict
//!
//! Each annotation resolves to exactly one of:
//!
//! - `Pass` — dispatcher exists, body is not a stub, exited 0.
//! - `Fail` — dispatcher exists, body is not a stub, exited non-zero
//!   (captures exit code + tail of stderr).
//! - `Stubbed` — dispatcher body calls `_pending_stub`. Not run.
//! - `MissingDispatcher` — annotation references a `test_*` function
//!   that is not defined in `tests/loom-test.sh`. Not run.
//!
//! Past-pass status is **not** persisted; every invocation re-runs the
//! verifier so the report reflects the current code-spec pair.
//!
//! ## Auxiliary findings (not per-criterion)
//!
//! The audit also surfaces two structural warnings that don't attach to
//! a single criterion:
//!
//! - **Unit-test masquerade** (R10) — a real dispatcher whose body runs
//!   `cargo test --lib` against a `::tests::` path (a `#[cfg(test)] mod
//!   tests` block inside the production crate). Suppressed by the
//!   per-annotation `@unit-ok` marker.
//! - **Orphan dispatcher** (R10) — a `test_<name>` defined in
//!   `tests/loom-test.sh` that no `[verify]` annotation references.
//!
//! ## Exit code
//!
//! `loom check --check=criteria` exits `0` when every criterion is
//! `Pass` or `Stubbed` and there are no error-severity findings, `1`
//! when any criterion is `Fail` / `MissingDispatcher` or there is any
//! error finding. `--strict` promotes stubs and warnings to errors.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum CriteriaError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("specs directory not found at {0}")]
    NoSpecsDir(PathBuf),
    #[error("tests/loom-test.sh not found at {0}")]
    NoDispatcher(PathBuf),
}

/// Parsed `[verify](tests/loom-test.sh::test_<fn>)` annotation.
#[derive(Debug, Clone)]
pub struct VerifyAnnotation {
    pub spec_file: PathBuf,
    /// 1-based line number where the annotation was found.
    pub line_no: usize,
    /// `test_<fn>` — the dispatcher function name (without the
    /// `tests/loom-test.sh::` prefix).
    pub dispatcher_fn: String,
    /// `@unit-ok` opt-out marker (R10, wx-2pbxe). Suppresses unit-test
    /// masquerade warnings for criteria that legitimately target unit logic.
    pub unit_ok: bool,
}

/// Per-criterion verdict produced by running the annotated dispatcher
/// (or by static inspection when it would be a stub / missing /
/// deliberately skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionVerdict {
    Pass,
    Fail {
        exit_code: i32,
        stderr_tail: String,
    },
    Stubbed,
    MissingDispatcher,
    /// Runner was deliberately skipped (e.g. `loom check criteria
    /// --no-run` for the structural pre-commit lint). Static checks
    /// still ran — stubs and missing dispatchers are reported under
    /// their own verdicts; this variant is only used for would-be-live
    /// entries that were not executed.
    Skipped,
}

impl CriterionVerdict {
    pub fn tag(&self) -> &'static str {
        match self {
            CriterionVerdict::Pass => "pass",
            CriterionVerdict::Fail { .. } => "fail",
            CriterionVerdict::Stubbed => "stubbed",
            CriterionVerdict::MissingDispatcher => "missing-dispatcher",
            CriterionVerdict::Skipped => "skipped",
        }
    }
}

/// One criterion's location and current verdict.
#[derive(Debug, Clone)]
pub struct CriterionResult {
    pub spec_file: PathBuf,
    pub line_no: usize,
    pub dispatcher_fn: String,
    pub verdict: CriterionVerdict,
}

/// Output of running a dispatcher function.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub exit_code: i32,
    pub stderr_tail: String,
}

/// Abstraction over "invoke `test_<fn>` in `tests/loom-test.sh` and
/// observe the result". Real callers use [`ShellRunner`]; tests inject
/// a recording fake so the audit logic can be exercised without a
/// `cargo test` round-trip.
pub trait DispatcherRunner {
    fn run(&self, fn_name: &str) -> RunOutcome;
}

/// Default runner: `bash <dispatcher_path> <fn_name>`. Captures stderr
/// and reports the last ~40 lines on failure.
pub struct ShellRunner {
    pub dispatcher_path: PathBuf,
}

impl ShellRunner {
    pub fn new(dispatcher_path: impl Into<PathBuf>) -> Self {
        Self {
            dispatcher_path: dispatcher_path.into(),
        }
    }
}

impl DispatcherRunner for ShellRunner {
    fn run(&self, fn_name: &str) -> RunOutcome {
        let output = Command::new("bash")
            .arg(&self.dispatcher_path)
            .arg(fn_name)
            .output();
        match output {
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                let tail = tail_lines(&stderr, 40);
                let exit_code = o.status.code().unwrap_or(-1);
                RunOutcome {
                    exit_code,
                    stderr_tail: tail,
                }
            }
            Err(e) => RunOutcome {
                exit_code: -1,
                stderr_tail: format!("failed to spawn `bash`: {e}"),
            },
        }
    }
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Full audit output: a verdict per `[verify]` annotation plus
/// non-per-criterion findings (orphan dispatchers, masquerade warnings).
#[derive(Debug, Default)]
pub struct AuditReport {
    pub criteria: Vec<CriterionResult>,
    pub findings: Vec<Finding>,
}

pub fn parse_verify_annotations(specs_dir: &Path) -> Result<Vec<VerifyAnnotation>, CriteriaError> {
    if !specs_dir.is_dir() {
        return Err(CriteriaError::NoSpecsDir(specs_dir.to_path_buf()));
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(specs_dir).map_err(|source| CriteriaError::ReadFile {
        path: specs_dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let body = fs::read_to_string(&path).map_err(|source| CriteriaError::ReadFile {
            path: path.clone(),
            source,
        })?;
        parse_verify_from_body(&path, &body, &mut out);
    }
    out.sort_by(|a, b| {
        a.spec_file
            .cmp(&b.spec_file)
            .then(a.line_no.cmp(&b.line_no))
    });
    Ok(out)
}

fn parse_verify_from_body(spec_file: &Path, body: &str, out: &mut Vec<VerifyAnnotation>) {
    for (idx, line) in body.lines().enumerate() {
        let marker = "[verify](tests/loom-test.sh::";
        if let Some(start) = line.find(marker) {
            let after = &line[start + marker.len()..];
            if let Some(end) = after.find(')') {
                let raw = &after[..end];
                let (fn_name, unit_ok) = match raw.split_once(" @unit-ok") {
                    Some((name, _)) => (name.trim(), true),
                    None => (raw.trim(), false),
                };
                out.push(VerifyAnnotation {
                    spec_file: spec_file.to_path_buf(),
                    line_no: idx + 1,
                    dispatcher_fn: fn_name.to_string(),
                    unit_ok,
                });
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct DispatcherIndex {
    /// Functions whose body calls `_pending_stub`.
    pub stubs: HashSet<String>,
    /// Functions whose body is a real test invocation.
    pub real: HashSet<String>,
    /// Per-function body text for masquerade inspection. Keyed by
    /// `test_<name>`; value is the lines between the opening `{` and
    /// the matching closing `}` (newline-joined).
    pub bodies: std::collections::HashMap<String, String>,
}

/// Collect every `test_<name>() { … }` symbol defined in `tests/loom-test.sh`,
/// bucket each as `_pending_stub` vs real, and capture each body so the
/// audit can inspect it for masquerade signatures.
pub fn parse_dispatcher(dispatcher_path: &Path) -> Result<DispatcherIndex, CriteriaError> {
    if !dispatcher_path.is_file() {
        return Err(CriteriaError::NoDispatcher(dispatcher_path.to_path_buf()));
    }
    let body = fs::read_to_string(dispatcher_path).map_err(|source| CriteriaError::ReadFile {
        path: dispatcher_path.to_path_buf(),
        source,
    })?;
    let mut out = DispatcherIndex::default();
    let lines: Vec<&str> = body.lines().collect();
    let starts: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim_start();
            let rest = trimmed.strip_prefix("test_")?;
            let paren_at = rest.find('(')?;
            let name_segment = &rest[..paren_at];
            if name_segment
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
            {
                return None;
            }
            Some((idx, format!("test_{name_segment}")))
        })
        .collect();
    for (i, (start, fn_name)) in starts.iter().enumerate() {
        let end = starts
            .get(i + 1)
            .map(|(next_start, _)| *next_start)
            .unwrap_or(lines.len());
        let body_text = lines[*start..end].join("\n");
        let is_stub = body_text.contains("_pending_stub");
        if is_stub {
            out.stubs.insert(fn_name.clone());
        } else {
            out.real.insert(fn_name.clone());
        }
        out.bodies.insert(fn_name.clone(), body_text);
    }
    Ok(out)
}

pub fn is_unit_masquerade(body: &str) -> bool {
    let mut saw_lib = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.contains(" --lib ")
            || trimmed.contains(" --lib\\")
            || trimmed.ends_with(" --lib")
        {
            saw_lib = true;
        }
        if saw_lib && trimmed.contains("::tests::") {
            return true;
        }
    }
    false
}

/// Run the full audit: per-criterion verdicts via `runner`, plus
/// auxiliary findings (orphans, masquerade) by static inspection.
///
/// Pass `runner: Some(&r)` to actually execute the live verifiers (the
/// FR14 default). Pass `None` for the structural-only pass used by the
/// pre-commit hook: stubs and missing dispatchers are still flagged,
/// orphans and masquerade still surface as findings, but real
/// dispatchers receive a `Skipped` verdict instead of being invoked.
pub fn audit<R: DispatcherRunner>(
    specs_dir: &Path,
    dispatcher_path: &Path,
    runner: Option<&R>,
) -> Result<AuditReport, CriteriaError> {
    let annotations = parse_verify_annotations(specs_dir)?;
    let index = parse_dispatcher(dispatcher_path)?;
    let mut report = AuditReport::default();
    let mut referenced: HashSet<String> = HashSet::new();

    for ann in &annotations {
        let fn_name = &ann.dispatcher_fn;
        referenced.insert(fn_name.clone());

        let verdict = if !index.stubs.contains(fn_name) && !index.real.contains(fn_name) {
            CriterionVerdict::MissingDispatcher
        } else if index.stubs.contains(fn_name) {
            CriterionVerdict::Stubbed
        } else {
            match runner {
                None => CriterionVerdict::Skipped,
                Some(r) => {
                    let outcome = r.run(fn_name);
                    if outcome.exit_code == 0 {
                        CriterionVerdict::Pass
                    } else {
                        CriterionVerdict::Fail {
                            exit_code: outcome.exit_code,
                            stderr_tail: outcome.stderr_tail,
                        }
                    }
                }
            }
        };

        report.criteria.push(CriterionResult {
            spec_file: ann.spec_file.clone(),
            line_no: ann.line_no,
            dispatcher_fn: fn_name.clone(),
            verdict,
        });

        // R10: masquerade is a structural finding about the dispatcher,
        // not part of the per-criterion verdict.
        if !ann.unit_ok
            && index.real.contains(fn_name)
            && let Some(body) = index.bodies.get(fn_name)
            && is_unit_masquerade(body)
        {
            report.findings.push(Finding {
                severity: Severity::Warning,
                location: format!("{}:{}", ann.spec_file.display(), ann.line_no),
                message: format!(
                    "unit-test masquerade: `{fn_name}` runs a `--lib` test whose \
                     path contains `::tests::` (production-crate unit test). \
                     Add `@unit-ok` to the annotation if the unit-level coverage \
                     is intentional.",
                ),
            });
        }
    }

    // R10: orphan dispatchers — defined but unreferenced.
    let mut all_fns: Vec<&String> = index.stubs.iter().chain(index.real.iter()).collect();
    all_fns.sort();
    for fn_name in all_fns {
        if !referenced.contains(fn_name) {
            report.findings.push(Finding {
                severity: Severity::Warning,
                location: format!("{}", dispatcher_path.display()),
                message: format!(
                    "orphan dispatcher `{fn_name}`: defined in the test runner \
                     but no `[verify]` annotation references it",
                ),
            });
        }
    }

    Ok(report)
}

/// Print per-criterion verdicts and auxiliary findings to stderr in a
/// stable format. Returns the process exit code.
///
/// Exit semantics:
/// - `0` when every criterion is `pass` (stubs allowed) and no
///   error-severity findings exist.
/// - `1` when any criterion is `fail` or `missing-dispatcher`, or any
///   finding is `Error` severity.
/// - In `--strict`, stubs and warning-severity findings also count
///   toward exit `1`.
pub fn report(report: &AuditReport, strict: bool) -> i32 {
    let mut hard_count = 0;
    let mut pass_count = 0;
    let mut fail_count = 0;
    let mut stub_count = 0;
    let mut missing_count = 0;
    let mut skipped_count = 0;

    for c in &report.criteria {
        let tag = match &c.verdict {
            CriterionVerdict::Pass => {
                pass_count += 1;
                "PASS"
            }
            CriterionVerdict::Fail { .. } => {
                fail_count += 1;
                hard_count += 1;
                "FAIL"
            }
            CriterionVerdict::Stubbed => {
                stub_count += 1;
                if strict {
                    hard_count += 1;
                }
                "STUB"
            }
            CriterionVerdict::MissingDispatcher => {
                missing_count += 1;
                hard_count += 1;
                "MISS"
            }
            CriterionVerdict::Skipped => {
                skipped_count += 1;
                "SKIP"
            }
        };
        eprintln!(
            "{tag} {file}:{line} {fn_name}",
            file = c.spec_file.display(),
            line = c.line_no,
            fn_name = c.dispatcher_fn,
        );
        if let CriterionVerdict::Fail {
            exit_code,
            stderr_tail,
        } = &c.verdict
        {
            eprintln!("     exit {exit_code}");
            for line in stderr_tail.lines() {
                eprintln!("     | {line}");
            }
        }
    }

    for f in &report.findings {
        let prefix = match (f.severity, strict) {
            (Severity::Error, _) => {
                hard_count += 1;
                "ERROR"
            }
            (Severity::Warning, true) => {
                hard_count += 1;
                "ERROR"
            }
            (Severity::Warning, false) => "WARN",
        };
        eprintln!(
            "{prefix} {location}: {msg}",
            location = f.location,
            msg = f.message
        );
    }

    eprintln!(
        "loom check --check=criteria: {total} criteria \
         ({pass_count} pass, {fail_count} fail, {stub_count} stub, \
         {missing_count} missing, {skipped_count} skipped); \
         {findings} findings ({hard_count} hard)",
        total = report.criteria.len(),
        findings = report.findings.len(),
    );

    if hard_count == 0 { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording fake runner. Returns a canned `RunOutcome` for each
    /// `test_*` name; unrecognised names default to exit 0.
    struct FakeRunner {
        outcomes: std::collections::HashMap<String, RunOutcome>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                outcomes: std::collections::HashMap::new(),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with(mut self, fn_name: &str, exit_code: i32, stderr: &str) -> Self {
            self.outcomes.insert(
                fn_name.to_string(),
                RunOutcome {
                    exit_code,
                    stderr_tail: stderr.to_string(),
                },
            );
            self
        }
    }

    impl DispatcherRunner for FakeRunner {
        fn run(&self, fn_name: &str) -> RunOutcome {
            self.calls.borrow_mut().push(fn_name.to_string());
            self.outcomes.get(fn_name).cloned().unwrap_or(RunOutcome {
                exit_code: 0,
                stderr_tail: String::new(),
            })
        }
    }

    fn write_spec(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        fs::write(&path, body).expect("write spec");
    }

    fn workspace_with(
        dispatcher_body: &str,
        spec_body: &str,
    ) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(&specs, "a.md", spec_body);
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(&dispatcher, dispatcher_body).expect("write dispatcher");
        (dir, specs, dispatcher)
    }

    #[test]
    fn parser_extracts_annotations_ignoring_checkbox_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        // FR14: checkboxes are no longer part of the spec syntax, but
        // the parser must still extract annotations from legacy specs
        // that happen to carry them.
        let body = "\
- First criterion
  [verify](tests/loom-test.sh::test_first)
- [x] Second criterion (legacy checkbox)
  [verify](tests/loom-test.sh::test_second)
- [ ] Third criterion (legacy checkbox)
  [verify](tests/loom-test.sh::test_third)
";
        write_spec(dir.path(), "demo.md", body);
        let ann = parse_verify_annotations(dir.path()).expect("parse");
        assert_eq!(ann.len(), 3);
        assert_eq!(ann[0].dispatcher_fn, "test_first");
        assert_eq!(ann[1].dispatcher_fn, "test_second");
        assert_eq!(ann[2].dispatcher_fn, "test_third");
    }

    #[test]
    fn dispatcher_parser_buckets_stubs_vs_real() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loom-test.sh");
        let body = "\
test_one() { _pending_stub one; }
test_two() {
    cargo test --lib foo
}
test_three() {
    _pending_stub three
}
";
        fs::write(&path, body).expect("write dispatcher");
        let index = parse_dispatcher(&path).expect("parse");
        assert!(index.stubs.contains("test_one"));
        assert!(index.stubs.contains("test_three"));
        assert!(index.real.contains("test_two"));
        assert_eq!(index.stubs.len() + index.real.len(), 3);
        assert!(index.bodies["test_two"].contains("cargo test"));
    }

    #[test]
    fn unit_masquerade_detects_lib_tests_path() {
        let body = "    cargo_run test -p loom-render --lib -- renderer::tests::pretty_mode";
        assert!(is_unit_masquerade(body));
    }

    #[test]
    fn unit_masquerade_skips_integration_tests() {
        let body = "    cargo_run test -p loom-driver --test logging -- run_default_output_shape";
        assert!(!is_unit_masquerade(body));
    }

    #[test]
    fn annotation_parser_extracts_unit_ok_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "- keep this verifier\n  [verify](tests/loom-test.sh::test_fn @unit-ok)\n";
        write_spec(dir.path(), "demo.md", body);
        let ann = parse_verify_annotations(dir.path()).expect("parse");
        assert_eq!(ann.len(), 1);
        assert_eq!(ann[0].dispatcher_fn, "test_fn");
        assert!(ann[0].unit_ok, "unit-ok marker must round-trip");
    }

    /// FR14 — a real, non-stub dispatcher that exits 0 yields a
    /// `Pass` verdict.
    #[test]
    fn audit_reports_pass_when_runner_returns_zero() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_a() { cargo_run test foo; }\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let runner = FakeRunner::new().with("test_a", 0, "");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert_eq!(r.criteria.len(), 1);
        assert_eq!(r.criteria[0].verdict, CriterionVerdict::Pass);
        assert_eq!(runner.calls.borrow().as_slice(), &["test_a".to_string()]);
    }

    /// FR14 — a real dispatcher that exits non-zero yields a `Fail`
    /// verdict carrying the exit code + stderr tail.
    #[test]
    fn audit_reports_fail_when_runner_returns_nonzero() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_a() { cargo_run test foo; }\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let runner = FakeRunner::new().with("test_a", 101, "assertion failed at line 5\n");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert_eq!(r.criteria.len(), 1);
        match &r.criteria[0].verdict {
            CriterionVerdict::Fail {
                exit_code,
                stderr_tail,
            } => {
                assert_eq!(*exit_code, 101);
                assert!(stderr_tail.contains("assertion failed"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    /// FR14 — a stubbed dispatcher yields `Stubbed` regardless of
    /// runner behavior. The runner is **not** invoked for stubs.
    #[test]
    fn audit_reports_stubbed_without_invoking_runner() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_a() { _pending_stub a; }\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let runner = FakeRunner::new();
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert_eq!(r.criteria.len(), 1);
        assert_eq!(r.criteria[0].verdict, CriterionVerdict::Stubbed);
        assert!(
            runner.calls.borrow().is_empty(),
            "stubbed dispatcher must not be executed: {:?}",
            runner.calls.borrow()
        );
    }

    /// FR14 — annotation referencing an undefined `test_*` function
    /// yields `MissingDispatcher`. Runner is not invoked.
    #[test]
    fn audit_reports_missing_dispatcher() {
        let (_dir, specs, dispatcher) = workspace_with(
            "# empty\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_nope)\n",
        );
        let runner = FakeRunner::new();
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert_eq!(r.criteria.len(), 1);
        assert_eq!(r.criteria[0].verdict, CriterionVerdict::MissingDispatcher);
        assert!(runner.calls.borrow().is_empty());
    }

    /// FR14 — mixed verdicts in one spec. Order follows annotation
    /// order (deterministic sort by spec_file, line_no).
    #[test]
    fn audit_mixed_verdicts_in_one_spec() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_passing() { cargo_run test passing; }\n\
             test_failing() { cargo_run test failing; }\n\
             test_stub() { _pending_stub stub; }\n",
            "- passes\n  [verify](tests/loom-test.sh::test_passing)\n\
             - fails\n  [verify](tests/loom-test.sh::test_failing)\n\
             - stubbed\n  [verify](tests/loom-test.sh::test_stub)\n\
             - missing\n  [verify](tests/loom-test.sh::test_gone)\n",
        );
        let runner =
            FakeRunner::new()
                .with("test_passing", 0, "")
                .with("test_failing", 1, "boom\n");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        let tags: Vec<&str> = r.criteria.iter().map(|c| c.verdict.tag()).collect();
        assert_eq!(tags, vec!["pass", "fail", "stubbed", "missing-dispatcher"]);
    }

    /// FR14 — exit code: `Fail` and `MissingDispatcher` are hard, but
    /// `Stubbed` is not by default.
    #[test]
    fn report_exit_code_zero_when_only_pass_and_stub() {
        let mut r = AuditReport::default();
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 1,
            dispatcher_fn: "test_a".into(),
            verdict: CriterionVerdict::Pass,
        });
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 2,
            dispatcher_fn: "test_b".into(),
            verdict: CriterionVerdict::Stubbed,
        });
        assert_eq!(report(&r, false), 0);
    }

    #[test]
    fn report_exit_code_one_on_fail() {
        let mut r = AuditReport::default();
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 1,
            dispatcher_fn: "test_a".into(),
            verdict: CriterionVerdict::Fail {
                exit_code: 1,
                stderr_tail: "boom".into(),
            },
        });
        assert_eq!(report(&r, false), 1);
    }

    #[test]
    fn report_exit_code_one_on_missing_dispatcher() {
        let mut r = AuditReport::default();
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 1,
            dispatcher_fn: "test_a".into(),
            verdict: CriterionVerdict::MissingDispatcher,
        });
        assert_eq!(report(&r, false), 1);
    }

    /// `--strict` promotes stubs to error.
    #[test]
    fn report_strict_promotes_stub_to_error() {
        let mut r = AuditReport::default();
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 1,
            dispatcher_fn: "test_a".into(),
            verdict: CriterionVerdict::Stubbed,
        });
        assert_eq!(report(&r, false), 0);
        assert_eq!(report(&r, true), 1);
    }

    /// R10 — masquerade warning still surfaces alongside the
    /// per-criterion verdict.
    #[test]
    fn audit_warns_on_unit_test_masquerade() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_unit() {\n    cargo_run test -p loom-render --lib -- renderer::tests::x\n}\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_unit)\n",
        );
        let runner = FakeRunner::new().with("test_unit", 0, "");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert!(
            r.findings
                .iter()
                .any(|f| f.severity == Severity::Warning && f.message.contains("masquerade")),
            "expected masquerade warning, got: {:?}",
            r.findings,
        );
    }

    #[test]
    fn audit_unit_ok_marker_silences_masquerade_warning() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_unit() {\n    cargo_run test -p loom-render --lib -- renderer::tests::x\n}\n",
            "- intentional unit test\n  [verify](tests/loom-test.sh::test_unit @unit-ok)\n",
        );
        let runner = FakeRunner::new().with("test_unit", 0, "");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert!(
            !r.findings.iter().any(|f| f.message.contains("masquerade")),
            "@unit-ok must silence masquerade warning: {:?}",
            r.findings,
        );
    }

    #[test]
    fn audit_warns_on_orphan_dispatcher() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_referenced() { cargo test r; }\n\
             test_orphan() { cargo test o; }\n",
            "- not yet\n  [verify](tests/loom-test.sh::test_referenced)\n",
        );
        let runner = FakeRunner::new().with("test_referenced", 0, "");
        let r = audit(&specs, &dispatcher, Some(&runner)).expect("audit");
        assert!(
            r.findings.iter().any(|f| {
                f.severity == Severity::Warning
                    && f.message.contains("orphan")
                    && f.message.contains("test_orphan")
            }),
            "expected orphan warning, got: {:?}",
            r.findings,
        );
    }

    /// FR14 — every invocation re-runs the verifier; the audit does
    /// not cache a previous pass. Witnessed by the runner being called
    /// once per `audit()` invocation.
    #[test]
    fn audit_does_not_cache_past_passes() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_a() { cargo_run test a; }\n",
            "- criterion\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let pass_runner = FakeRunner::new().with("test_a", 0, "");
        let r1 = audit(&specs, &dispatcher, Some(&pass_runner)).expect("audit");
        assert_eq!(r1.criteria[0].verdict, CriterionVerdict::Pass);

        // Second invocation with a different runner outcome — verdict
        // must flip without any persistence carrying over.
        let fail_runner = FakeRunner::new().with("test_a", 2, "regression\n");
        let r2 = audit(&specs, &dispatcher, Some(&fail_runner)).expect("audit");
        assert!(matches!(
            r2.criteria[0].verdict,
            CriterionVerdict::Fail { .. }
        ));
    }

    /// `--no-run` (None runner) skips invoking live verifiers but
    /// keeps the structural verdicts: stubs and missing-dispatchers
    /// still flagged. Real dispatchers become `Skipped`.
    #[test]
    fn audit_with_no_runner_skips_live_run() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_real() { cargo_run test real; }\n\
             test_stub() { _pending_stub stub; }\n",
            "- real\n  [verify](tests/loom-test.sh::test_real)\n\
             - stubbed\n  [verify](tests/loom-test.sh::test_stub)\n\
             - missing\n  [verify](tests/loom-test.sh::test_gone)\n",
        );
        let r = audit::<FakeRunner>(&specs, &dispatcher, None).expect("audit");
        let tags: Vec<&str> = r.criteria.iter().map(|c| c.verdict.tag()).collect();
        assert_eq!(tags, vec!["skipped", "stubbed", "missing-dispatcher"]);
    }

    /// `--no-run` mode: exit code stays 0 when only stubs/skipped
    /// (default severity); 1 on missing-dispatcher because that's a
    /// structural hard error regardless of run mode.
    #[test]
    fn report_skipped_does_not_count_as_hard_error() {
        let mut r = AuditReport::default();
        r.criteria.push(CriterionResult {
            spec_file: PathBuf::from("specs/x.md"),
            line_no: 1,
            dispatcher_fn: "test_a".into(),
            verdict: CriterionVerdict::Skipped,
        });
        assert_eq!(report(&r, false), 0);
    }
}
