//! `loom doctor --check=criteria` — spec ↔ test stub-and-integration audit.
//!
//! Walks every `specs/*.md` for `[verify](tests/loom-test.sh::test_<fn>)`
//! annotations, opens the dispatcher in `tests/loom-test.sh`, and reports
//! violations against the four spec-defined conditions:
//!
//! 1. **Stub criteria with checked boxes** — a `[x]` criterion whose
//!    dispatcher still calls `_pending_stub`. Hard error.
//! 2. **Unit-test masquerade** — a checked criterion whose dispatcher
//!    runs a `--lib` profile test whose function path contains
//!    `::tests::`, i.e. a `#[cfg(test)] mod tests` block inside the
//!    production crate. Warning by default, error in `--strict`. R10
//!    (wx-2pbxe). Suppressed by the per-annotation `@unit-ok` marker:
//!    `[verify](tests/loom-test.sh::test_fn @unit-ok)`.
//! 3. **Missing dispatcher** — `[verify](tests/loom-test.sh::test_X)`
//!    where `test_X` is not defined. Hard error.
//! 4. **Orphan stubs** — a function in `tests/loom-test.sh` that no
//!    criterion references. Warning. R10 (wx-2pbxe).
//!
//! `loom doctor --check=criteria` exits 0 when no violations are found,
//! `1` for any hard error, `2` for arg parsing problems.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Severity of a doctor finding. Hard errors fail the exit code; the
/// strict flag promotes warnings to errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One finding from the audit. `location` is the spec file + line
/// number that owned the criterion (or the dispatcher file).
#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub location: String,
    pub message: String,
}

/// Top-level errors from running the audit (distinct from `Finding` —
/// these are about the audit itself failing, not the audit reporting
/// violations).
#[derive(Debug, Error)]
pub enum DoctorError {
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
    /// `tests/loom-test.sh::test_<fn>` — the path part is the
    /// dispatcher file, the `::test_<fn>` part is the function name.
    pub dispatcher_fn: String,
    /// `true` when the preceding criterion checkbox is `[x]`.
    pub checked: bool,
    /// `true` when the annotation carries the `@unit-ok` opt-out
    /// marker (R10, wx-2pbxe). Suppresses unit-test masquerade
    /// warnings for criteria that legitimately target unit logic.
    pub unit_ok: bool,
}

/// Walk `specs_dir` for `.md` files and extract every `[verify]`
/// annotation along with its preceding `[ ]` / `[x]` state. The
/// regex-free parser scans line-by-line, tracking the most recent
/// criterion bullet's checkbox state.
pub fn parse_verify_annotations(specs_dir: &Path) -> Result<Vec<VerifyAnnotation>, DoctorError> {
    if !specs_dir.is_dir() {
        return Err(DoctorError::NoSpecsDir(specs_dir.to_path_buf()));
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(specs_dir).map_err(|source| DoctorError::ReadFile {
        path: specs_dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let body = fs::read_to_string(&path).map_err(|source| DoctorError::ReadFile {
            path: path.clone(),
            source,
        })?;
        parse_verify_from_body(&path, &body, &mut out);
    }
    Ok(out)
}

fn parse_verify_from_body(spec_file: &Path, body: &str, out: &mut Vec<VerifyAnnotation>) {
    // Track the most recent criterion's checkbox state. A criterion
    // bullet looks like `- [ ] …` or `- [x] …` (any indent); we keep
    // the last-seen value and pair it with the next `[verify](...)`
    // annotation we encounter.
    let mut last_checked: Option<bool> = None;
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("- [")
            .or_else(|| trimmed.strip_prefix("* ["))
        {
            // Two-byte chars: `[ ]` or `[x]` or `[X]`. Inspect first byte.
            let first = rest.chars().next();
            if first == Some(' ') {
                last_checked = Some(false);
            } else if first == Some('x') || first == Some('X') {
                last_checked = Some(true);
            }
        }
        // `[verify](tests/loom-test.sh::test_foo)` — find the substring
        // and pull the function name out.
        let marker = "[verify](tests/loom-test.sh::";
        if let Some(start) = line.find(marker) {
            let after = &line[start + marker.len()..];
            if let Some(end) = after.find(')') {
                let raw = &after[..end];
                // R10 — `@unit-ok` is a space-separated trailing marker
                // that suppresses the unit-masquerade warning for the
                // criterion. Strip it before recording the fn name.
                let (fn_name, unit_ok) = match raw.split_once(" @unit-ok") {
                    Some((name, _)) => (name.trim(), true),
                    None => (raw.trim(), false),
                };
                out.push(VerifyAnnotation {
                    spec_file: spec_file.to_path_buf(),
                    line_no: idx + 1,
                    dispatcher_fn: fn_name.to_string(),
                    checked: last_checked.unwrap_or(false),
                    unit_ok,
                });
            }
        }
    }
}

/// Parsed dispatcher map: per-function classification plus the body
/// text the audit walks to detect unit-test masquerade (R10).
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
///
/// Bodies are captured by reading forward from the function header until
/// the next `test_<name>()` header or the end of file. Brace counting in
/// real-world bash mismatches because of `{`/`}` characters inside
/// strings, here-docs, and comments — line-based segmentation is simpler
/// and correct enough for the heuristic checks the audit needs.
pub fn parse_dispatcher(dispatcher_path: &Path) -> Result<DispatcherIndex, DoctorError> {
    if !dispatcher_path.is_file() {
        return Err(DoctorError::NoDispatcher(dispatcher_path.to_path_buf()));
    }
    let body = fs::read_to_string(dispatcher_path).map_err(|source| DoctorError::ReadFile {
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
            // Reject names that have spaces or other separators before the
            // paren — those are call sites (`test_foo arg`), not headers.
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

/// Heuristic: does the dispatcher body look like it runs a unit-test
/// masquerade — a `--lib` profile test whose path contains `::tests::`?
/// That's the signal that the criterion is verified by a
/// `#[cfg(test)] mod tests {}` block inside the production crate
/// rather than an integration test. R10 (wx-2pbxe).
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
        // The `::tests::` segment is the giveaway — `cargo test` paths
        // include the module chain, and a unit-test inside the
        // canonical `#[cfg(test)] mod tests` lives at `<crate>::tests::<fn>`
        // (or `<module>::tests::<fn>` for nested modules).
        if saw_lib && trimmed.contains("::tests::") {
            return true;
        }
    }
    false
}

/// Run the criteria audit. Returns the collected `Finding`s — empty
/// vec means clean.
pub fn audit(specs_dir: &Path, dispatcher_path: &Path) -> Result<Vec<Finding>, DoctorError> {
    let annotations = parse_verify_annotations(specs_dir)?;
    let index = parse_dispatcher(dispatcher_path)?;
    let mut findings = Vec::new();
    let mut referenced: HashSet<String> = HashSet::new();
    for ann in &annotations {
        let fn_name = &ann.dispatcher_fn;
        referenced.insert(fn_name.clone());
        let where_at = format!("{}:{}", ann.spec_file.display(), ann.line_no,);
        if !index.stubs.contains(fn_name) && !index.real.contains(fn_name) {
            findings.push(Finding {
                severity: Severity::Error,
                location: where_at,
                message: format!("missing dispatcher `{fn_name}` (referenced in `[verify]`)",),
            });
            continue;
        }
        if ann.checked && index.stubs.contains(fn_name) {
            findings.push(Finding {
                severity: Severity::Error,
                location: where_at.clone(),
                message: format!(
                    "criterion is checked `[x]` but dispatcher `{fn_name}` \
                     still calls `_pending_stub`",
                ),
            });
        }
        // Condition 2 (R10): unit-test masquerade. Skip stub
        // dispatchers — by definition they don't run anything — and
        // honor the `@unit-ok` opt-out.
        if !ann.unit_ok
            && index.real.contains(fn_name)
            && let Some(body) = index.bodies.get(fn_name)
            && is_unit_masquerade(body)
        {
            findings.push(Finding {
                severity: Severity::Warning,
                location: where_at,
                message: format!(
                    "unit-test masquerade: `{fn_name}` runs a `--lib` test whose \
                     path contains `::tests::` (production-crate unit test). \
                     Add `@unit-ok` to the annotation if the unit-level coverage \
                     is intentional.",
                ),
            });
        }
    }
    // Condition 4 (R10): orphan stubs/reals. Any dispatcher function
    // that no `[verify]` annotation references is dead weight.
    let mut all_fns: Vec<&String> = index.stubs.iter().chain(index.real.iter()).collect();
    all_fns.sort();
    for fn_name in all_fns {
        if !referenced.contains(fn_name) {
            findings.push(Finding {
                severity: Severity::Warning,
                location: format!("{}", dispatcher_path.display()),
                message: format!(
                    "orphan dispatcher `{fn_name}`: defined in the test runner \
                     but no `[verify]` annotation references it",
                ),
            });
        }
    }
    Ok(findings)
}

/// Print findings to stderr in a stable format. Returns the exit
/// code: `0` if no errors, `1` otherwise.
pub fn report(findings: &[Finding], strict: bool) -> i32 {
    let mut hard_count = 0;
    for f in findings {
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
    if findings.is_empty() {
        eprintln!("loom doctor: no violations");
    } else {
        eprintln!(
            "loom doctor: {n} findings ({hard_count} hard)",
            n = findings.len(),
        );
    }
    if hard_count == 0 { 0 } else { 1 }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    fn write_spec(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        fs::write(&path, body).expect("write spec");
    }

    #[test]
    fn parser_extracts_checkbox_and_annotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "\
- [x] First criterion
  [verify](tests/loom-test.sh::test_first)
- [ ] Second criterion
  [verify](tests/loom-test.sh::test_second)
";
        write_spec(dir.path(), "demo.md", body);
        let ann = parse_verify_annotations(dir.path()).expect("parse");
        assert_eq!(ann.len(), 2);
        assert_eq!(ann[0].dispatcher_fn, "test_first");
        assert!(ann[0].checked);
        assert_eq!(ann[1].dispatcher_fn, "test_second");
        assert!(!ann[1].checked);
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
        // R10 — body capture preserves the cargo invocation.
        assert!(index.bodies["test_two"].contains("cargo test"));
    }

    /// R10 — masquerade heuristic flags a `--lib` + `::tests::` pair.
    #[test]
    fn unit_masquerade_detects_lib_tests_path() {
        let body = "    cargo_run test -p loom-render --lib -- renderer::tests::pretty_mode";
        assert!(is_unit_masquerade(body));
    }

    /// R10 — integration test (`--test <name>` or no `--lib`) does not
    /// trigger the masquerade signal.
    #[test]
    fn unit_masquerade_skips_integration_tests() {
        let body = "    cargo_run test -p loom-driver --test logging -- run_default_output_shape";
        assert!(!is_unit_masquerade(body));
    }

    /// R10 — `@unit-ok` marker round-trips through the annotation parser.
    #[test]
    fn annotation_parser_extracts_unit_ok_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "- [x] keep this checked\n  [verify](tests/loom-test.sh::test_fn @unit-ok)\n";
        write_spec(dir.path(), "demo.md", body);
        let ann = parse_verify_annotations(dir.path()).expect("parse");
        assert_eq!(ann.len(), 1);
        assert_eq!(ann[0].dispatcher_fn, "test_fn");
        assert!(ann[0].unit_ok, "unit-ok marker must round-trip");
    }

    /// R10 — audit emits a Warning when the dispatcher body shows the
    /// masquerade signal.
    #[test]
    fn audit_warns_on_unit_test_masquerade() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "a.md",
            "- [x] checked\n  [verify](tests/loom-test.sh::test_unit)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(
            &dispatcher,
            "test_unit() {\n    cargo_run test -p loom-render --lib -- renderer::tests::x\n}\n",
        )
        .expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Warning && f.message.contains("masquerade")),
            "expected masquerade warning, got: {findings:?}",
        );
    }

    /// R10 — `@unit-ok` opt-out silences the masquerade warning.
    #[test]
    fn audit_unit_ok_marker_silences_masquerade_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "a.md",
            "- [x] intentional unit test\n  [verify](tests/loom-test.sh::test_unit @unit-ok)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(
            &dispatcher,
            "test_unit() {\n    cargo_run test -p loom-render --lib -- renderer::tests::x\n}\n",
        )
        .expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert!(
            !findings.iter().any(|f| f.message.contains("masquerade")),
            "@unit-ok must silence masquerade warning: {findings:?}",
        );
    }

    /// R10 — orphan stubs in the dispatcher emit a Warning.
    #[test]
    fn audit_warns_on_orphan_dispatcher() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        // The spec references one dispatcher; the dispatcher defines two.
        // The unreferenced one is an orphan.
        write_spec(
            &specs,
            "a.md",
            "- [ ] not yet\n  [verify](tests/loom-test.sh::test_referenced)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(
            &dispatcher,
            "test_referenced() { _pending_stub r; }\n\
             test_orphan() { _pending_stub o; }\n",
        )
        .expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert!(
            findings.iter().any(|f| {
                f.severity == Severity::Warning
                    && f.message.contains("orphan")
                    && f.message.contains("test_orphan")
            }),
            "expected orphan warning, got: {findings:?}",
        );
    }

    #[test]
    fn audit_flags_checked_stub_as_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "a.md",
            "- [x] this is checked\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(&dispatcher, "test_a() { _pending_stub a; }\n").expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(
            findings[0].message.contains("checked")
                && findings[0].message.contains("_pending_stub"),
            "{}",
            findings[0].message,
        );
    }

    #[test]
    fn audit_flags_missing_dispatcher_as_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "a.md",
            "- [ ] not yet checked\n  [verify](tests/loom-test.sh::test_does_not_exist)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(&dispatcher, "# empty\n").expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("missing dispatcher"));
    }

    #[test]
    fn audit_clean_when_real_dispatcher_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs).expect("specs dir");
        write_spec(
            &specs,
            "a.md",
            "- [x] all good\n  [verify](tests/loom-test.sh::test_a)\n",
        );
        let dispatcher = dir.path().join("loom-test.sh");
        fs::write(&dispatcher, "test_a() {\n    cargo test foo\n}\n").expect("write");
        let findings = audit(&specs, &dispatcher).expect("audit");
        assert!(findings.is_empty(), "expected clean, got: {findings:?}");
    }
}
