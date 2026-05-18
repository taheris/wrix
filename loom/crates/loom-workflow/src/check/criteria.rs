//! `loom check criteria` — spec ↔ test verifier audit (FR14).
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
//! The audit also surfaces one structural warning that doesn't attach to
//! a single criterion:
//!
//! - **Orphan dispatcher** — a `test_<name>` defined in
//!   `tests/loom-test.sh` that no `[verify]` annotation references.
//!
//! ## Exit code
//!
//! `loom check criteria` exits `0` when every criterion is
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
///
/// `Sync` bound: `audit_filtered` dispatches each live runner call on a
/// worker thread so a 280-criterion audit overlaps cargo per-invocation
/// overhead instead of paying it serially.
pub trait DispatcherRunner: Sync {
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

/// Wraps [`ShellRunner`] with a cache populated by a single up-front
/// `cargo nextest` batch. Any dispatcher whose body the parser can
/// resolve to a set of cargo-test entries gets its verdict from the
/// batched run instead of paying the per-dispatcher bash + cargo cost.
/// Anything the parser bounces (multi-line shell, `nix` invocations,
/// `--test-threads=1` serial tests) falls through to the bash runner.
pub struct BatchedShellRunner {
    fallback: ShellRunner,
    cache: std::collections::HashMap<String, RunOutcome>,
}

impl BatchedShellRunner {
    /// Construct, batch all parseable dispatchers via nextest, and
    /// populate the cache. `index` carries the per-dispatcher body
    /// text; `helpers` is the helper-function map from
    /// [`cargo_dispatch::parse_helpers`]. `workspace_dir` is the
    /// workspace root (cargo's `Cargo.toml` parent).
    pub fn new(
        dispatcher_path: impl Into<std::path::PathBuf>,
        workspace_dir: &Path,
        index: &DispatcherIndex,
        candidate_fns: &[&str],
    ) -> Self {
        let fallback = ShellRunner::new(dispatcher_path);
        let helpers = super::cargo_dispatch::parse_helpers(&index.source);
        let mut per_fn: std::collections::HashMap<String, super::cargo_dispatch::Invocation> =
            std::collections::HashMap::new();
        for fn_name in candidate_fns {
            if !index.real.contains(*fn_name) {
                continue;
            }
            let Some(body) = index.bodies.get(*fn_name) else {
                continue;
            };
            if let Some(inv) = super::cargo_dispatch::parse_dispatch(body, &helpers) {
                per_fn.insert((*fn_name).to_string(), inv);
            }
        }
        let mut all_entries: Vec<super::cargo_dispatch::TestEntry> = Vec::new();
        for inv in per_fn.values() {
            for entry in &inv.entries {
                all_entries.push(entry.clone());
            }
        }
        let mut cache = std::collections::HashMap::new();
        if !all_entries.is_empty() {
            match super::cargo_dispatch::run_batch(workspace_dir, &all_entries) {
                Ok(outcome) => {
                    let stderr_tail = tail_lines(&outcome.stderr, 40);
                    for (fn_name, inv) in &per_fn {
                        let any_failed = inv
                            .entries
                            .iter()
                            .any(|e| outcome.failed.contains(&e.test_name));
                        let verdict = if any_failed {
                            // nextest's interleaved stderr is the best
                            // we can give without a per-test re-run;
                            // the bash fallback is still an option for
                            // anyone who wants clean per-test output.
                            RunOutcome {
                                exit_code: 1,
                                stderr_tail: stderr_tail.clone(),
                            }
                        } else {
                            RunOutcome {
                                exit_code: 0,
                                stderr_tail: String::new(),
                            }
                        };
                        cache.insert(fn_name.clone(), verdict);
                    }
                }
                Err(err) => {
                    // nextest unavailable (cargo-nextest not on PATH)
                    // or invocation failed — leave the cache empty so
                    // everything falls back to the bash runner. The
                    // audit still completes; it just runs at the old
                    // serial speed.
                    eprintln!(
                        "warning: cargo nextest batch failed ({err}); falling back to per-dispatcher bash"
                    );
                }
            }
        }
        Self { fallback, cache }
    }
}

impl DispatcherRunner for BatchedShellRunner {
    fn run(&self, fn_name: &str) -> RunOutcome {
        if let Some(cached) = self.cache.get(fn_name) {
            return cached.clone();
        }
        self.fallback.run(fn_name)
    }
}

/// Full audit output: a verdict per `[verify]` annotation plus the
/// orphan-dispatcher finding.
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
                out.push(VerifyAnnotation {
                    spec_file: spec_file.to_path_buf(),
                    line_no: idx + 1,
                    dispatcher_fn: after[..end].trim().to_string(),
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
    /// Verbatim body of each `test_<name>` (lines between header and the
    /// next function header). Used by the nextest-batch fast path to
    /// recognise dispatchers whose body is a single cargo invocation.
    pub bodies: std::collections::HashMap<String, String>,
    /// Raw source of the whole dispatcher file. Needed so the cargo-
    /// invocation parser can resolve `*_cargo_test()` helpers.
    pub source: String,
}

/// Collect every `test_<name>() { … }` symbol defined in `tests/loom-test.sh`
/// and bucket each as `_pending_stub` vs real.
pub fn parse_dispatcher(dispatcher_path: &Path) -> Result<DispatcherIndex, CriteriaError> {
    if !dispatcher_path.is_file() {
        return Err(CriteriaError::NoDispatcher(dispatcher_path.to_path_buf()));
    }
    let body = fs::read_to_string(dispatcher_path).map_err(|source| CriteriaError::ReadFile {
        path: dispatcher_path.to_path_buf(),
        source,
    })?;
    let mut out = DispatcherIndex {
        source: body.clone(),
        ..DispatcherIndex::default()
    };
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

/// Run the full audit: per-criterion verdicts via `runner`, plus
/// the orphan-dispatcher finding by static inspection.
///
/// Pass `runner: Some(&r)` to actually execute the live verifiers (the
/// FR14 default). Pass `None` for the structural-only pass used by the
/// pre-commit hook: stubs, missing dispatchers, and orphan dispatchers
/// are still flagged, but real dispatchers receive a `Skipped` verdict
/// instead of being invoked.
pub fn audit<R: DispatcherRunner>(
    specs_dir: &Path,
    dispatcher_path: &Path,
    runner: Option<&R>,
) -> Result<AuditReport, CriteriaError> {
    audit_filtered(specs_dir, dispatcher_path, runner, None)
}

/// Like [`audit`] but optionally narrows the per-criterion walk to a
/// subset of spec files. The orphan-dispatcher check still uses the
/// global annotation set so it does not flag dispatchers covered by
/// out-of-scope specs.
///
/// `spec_filter: Some(set)` keeps only annotations whose `spec_file`
/// appears in `set`. `None` is identical to [`audit`].
pub fn audit_filtered<R: DispatcherRunner>(
    specs_dir: &Path,
    dispatcher_path: &Path,
    runner: Option<&R>,
    spec_filter: Option<&HashSet<PathBuf>>,
) -> Result<AuditReport, CriteriaError> {
    let all_annotations = parse_verify_annotations(specs_dir)?;
    let index = parse_dispatcher(dispatcher_path)?;
    let mut report = AuditReport::default();
    let mut globally_referenced: HashSet<String> = HashSet::new();
    for ann in &all_annotations {
        globally_referenced.insert(ann.dispatcher_fn.clone());
    }

    let in_scope: Vec<&VerifyAnnotation> = all_annotations
        .iter()
        .filter(|ann| match spec_filter {
            None => true,
            Some(set) => set.contains(&ann.spec_file),
        })
        .collect();

    // First pass: classify each criterion. Stubs / missing dispatchers /
    // no-runner skip get an immediate verdict; only "real" dispatchers
    // need a live invocation.
    enum Classification {
        Immediate(CriterionVerdict),
        Run,
    }
    let classified: Vec<Classification> = in_scope
        .iter()
        .map(|ann| {
            let fn_name = &ann.dispatcher_fn;
            if !index.stubs.contains(fn_name) && !index.real.contains(fn_name) {
                Classification::Immediate(CriterionVerdict::MissingDispatcher)
            } else if index.stubs.contains(fn_name) {
                Classification::Immediate(CriterionVerdict::Stubbed)
            } else if runner.is_none() {
                Classification::Immediate(CriterionVerdict::Skipped)
            } else {
                Classification::Run
            }
        })
        .collect();

    // Second pass: run the live dispatchers in parallel. Cargo's per-
    // invocation overhead (~100ms manifest + resolve) dominated when this
    // loop was sequential — 280 criteria × ~250ms ≈ 1m+ wall time. With a
    // worker pool sized to logical CPUs, the audit overlaps that overhead
    // across cores.
    let mut outcomes: std::collections::HashMap<usize, RunOutcome> =
        std::collections::HashMap::new();
    if let Some(r) = runner {
        let to_run: Vec<(usize, &str)> = classified
            .iter()
            .zip(in_scope.iter())
            .enumerate()
            .filter_map(|(idx, (cls, ann))| match cls {
                Classification::Run => Some((idx, ann.dispatcher_fn.as_str())),
                Classification::Immediate(_) => None,
            })
            .collect();

        if !to_run.is_empty() {
            let par = std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(4);
            let chunk_size = to_run.len().div_ceil(par.max(1));
            std::thread::scope(|s| {
                let handles: Vec<_> = to_run
                    .chunks(chunk_size)
                    .map(|chunk| {
                        s.spawn(move || -> Vec<(usize, RunOutcome)> {
                            chunk
                                .iter()
                                .map(|(idx, fn_name)| (*idx, r.run(fn_name)))
                                .collect()
                        })
                    })
                    .collect();
                for h in handles {
                    match h.join() {
                        Ok(chunk) => outcomes.extend(chunk),
                        // Propagate the original panic so the failure
                        // surface matches a sequential run; the audit's
                        // contract is "verdict per criterion or hard
                        // crash", not "swallow worker panic".
                        Err(payload) => std::panic::resume_unwind(payload),
                    }
                }
            });
        }
    }

    // Third pass: assemble verdicts in annotation order.
    for (idx, (ann, cls)) in in_scope.iter().zip(classified).enumerate() {
        let fn_name = &ann.dispatcher_fn;
        let verdict = match cls {
            Classification::Immediate(v) => v,
            Classification::Run => match outcomes.remove(&idx) {
                Some(outcome) => match outcome.exit_code {
                    0 => CriterionVerdict::Pass,
                    // POSIX/Automake convention: exit 77 means the
                    // dispatcher deliberately skipped (e.g. a tool it
                    // depends on is not on PATH in this environment).
                    77 => CriterionVerdict::Skipped,
                    code => CriterionVerdict::Fail {
                        exit_code: code,
                        stderr_tail: outcome.stderr_tail,
                    },
                },
                // Defensive: every `Classification::Run` is funnelled
                // into the parallel `to_run` list above. A missing
                // outcome means a worker silently dropped the slot —
                // surface as a hard fail with a synthetic exit code so
                // the audit stays observable.
                None => CriterionVerdict::Fail {
                    exit_code: -1,
                    stderr_tail: format!(
                        "internal: dispatcher worker produced no outcome for {fn_name}"
                    ),
                },
            },
        };

        report.criteria.push(CriterionResult {
            spec_file: ann.spec_file.clone(),
            line_no: ann.line_no,
            dispatcher_fn: fn_name.clone(),
            verdict,
        });
    }

    // Orphan dispatchers — defined but unreferenced. Use the global
    // annotation set so a scoped audit (e.g. `--bead`) does not flag
    // dispatchers that other specs reference.
    let mut all_fns: Vec<&String> = index.stubs.iter().chain(index.real.iter()).collect();
    all_fns.sort();
    for fn_name in all_fns {
        if !globally_referenced.contains(fn_name) {
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
        "loom check criteria: {total} criteria \
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
    use std::sync::Mutex;

    /// Recording fake runner. Returns a canned `RunOutcome` for each
    /// `test_*` name; unrecognised names default to exit 0. `Mutex`
    /// (not `RefCell`) because the runner trait requires `Sync` so the
    /// audit can dispatch in parallel.
    struct FakeRunner {
        outcomes: std::collections::HashMap<String, RunOutcome>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                outcomes: std::collections::HashMap::new(),
                calls: Mutex::new(Vec::new()),
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

        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("fake runner call log poisoned")
                .clone()
        }
    }

    impl DispatcherRunner for FakeRunner {
        fn run(&self, fn_name: &str) -> RunOutcome {
            self.calls
                .lock()
                .expect("fake runner call log poisoned")
                .push(fn_name.to_string());
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
        assert_eq!(runner.calls().as_slice(), &["test_a".to_string()]);
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
            runner.calls().is_empty(),
            "stubbed dispatcher must not be executed: {:?}",
            runner.calls()
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
        assert!(runner.calls().is_empty());
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

    /// `--bead` / `--diff` scope narrows the criteria walk to the
    /// supplied spec files. Annotations from out-of-scope specs are
    /// dropped from the per-criterion report.
    #[test]
    fn audit_filtered_keeps_only_in_scope_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "in.md",
            "- in scope\n  [verify](tests/loom-test.sh::test_in)\n",
        );
        write_spec(
            &specs,
            "out.md",
            "- out of scope\n  [verify](tests/loom-test.sh::test_out)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(
            &dispatcher,
            "test_in() { cargo_run test in; }\n\
             test_out() { cargo_run test out; }\n",
        )
        .expect("write dispatcher");

        let runner = FakeRunner::new()
            .with("test_in", 0, "")
            .with("test_out", 0, "");
        let mut filter = HashSet::new();
        filter.insert(specs.join("in.md"));
        let r = audit_filtered(&specs, &dispatcher, Some(&runner), Some(&filter)).expect("audit");

        let fns: Vec<&String> = r.criteria.iter().map(|c| &c.dispatcher_fn).collect();
        assert_eq!(fns, vec!["test_in"]);
        assert_eq!(
            runner.calls().as_slice(),
            &["test_in".to_string()],
            "out-of-scope dispatchers must not run",
        );
    }

    /// Scope-narrowed audit must NOT flag the out-of-scope spec's
    /// dispatcher as orphan: orphan detection consults the global
    /// annotation set so a per-bead audit doesn't emit false drift.
    #[test]
    fn audit_filtered_does_not_flag_dispatchers_referenced_by_out_of_scope_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "in.md",
            "- in\n  [verify](tests/loom-test.sh::test_in)\n",
        );
        write_spec(
            &specs,
            "out.md",
            "- out\n  [verify](tests/loom-test.sh::test_out)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(
            &dispatcher,
            "test_in() { cargo_run test in; }\n\
             test_out() { cargo_run test out; }\n",
        )
        .expect("write dispatcher");

        let runner = FakeRunner::new();
        let mut filter = HashSet::new();
        filter.insert(specs.join("in.md"));
        let r = audit_filtered(&specs, &dispatcher, Some(&runner), Some(&filter)).expect("audit");

        assert!(
            !r.findings.iter().any(|f| f.message.contains("orphan")),
            "test_out is referenced by out.md; scoped audit must not flag it as orphan: {:?}",
            r.findings,
        );
    }

    /// `audit_filtered(_, _, _, None)` must be observably identical to
    /// the legacy `audit(...)` entry point.
    #[test]
    fn audit_filtered_none_matches_legacy_audit() {
        let (_dir, specs, dispatcher) = workspace_with(
            "test_a() { cargo_run test a; }\n",
            "- one\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let runner_a = FakeRunner::new().with("test_a", 0, "");
        let runner_b = FakeRunner::new().with("test_a", 0, "");
        let legacy = audit(&specs, &dispatcher, Some(&runner_a)).expect("audit");
        let filtered =
            audit_filtered(&specs, &dispatcher, Some(&runner_b), None).expect("audit_filtered");
        let legacy_tags: Vec<&str> = legacy.criteria.iter().map(|c| c.verdict.tag()).collect();
        let filtered_tags: Vec<&str> = filtered.criteria.iter().map(|c| c.verdict.tag()).collect();
        assert_eq!(legacy_tags, filtered_tags);
    }
}
