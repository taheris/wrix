//! `loom doctor --check=criteria` — spec ↔ test stub-and-integration audit.
//!
//! Walks every `specs/*.md` for `[verify](tests/loom-test.sh::test_<fn>)`
//! annotations, opens the dispatcher in `tests/loom-test.sh`, and reports
//! violations against the four spec-defined conditions:
//!
//! 1. **Stub criteria with checked boxes** — a `[x]` criterion whose
//!    dispatcher still calls `_pending_stub`. Hard error.
//! 2. **Unit-test masquerade** — a checked criterion whose dispatcher
//!    targets a `#[cfg(test)] mod tests` symbol inside the production
//!    crate. Warning by default, error in `--strict`. **Not implemented
//!    in this MVP** — requires Rust AST inspection; deferred to a
//!    follow-up. Will surface as `loom doctor --check=criteria-strict`.
//! 3. **Missing dispatcher** — `[verify](tests/loom-test.sh::test_X)`
//!    where `test_X` is not defined. Hard error.
//! 4. **Orphan stubs** — a function in `tests/loom-test.sh` that no
//!    criterion references. **Not implemented in this MVP** — requires
//!    every annotation parsed across every spec; the framework is here
//!    and the check is a small follow-up.
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
                let fn_name = &after[..end];
                out.push(VerifyAnnotation {
                    spec_file: spec_file.to_path_buf(),
                    line_no: idx + 1,
                    dispatcher_fn: fn_name.to_string(),
                    checked: last_checked.unwrap_or(false),
                });
            }
        }
    }
}

/// Collect every `test_<name>() { … }` symbol defined in `tests/loom-test.sh`
/// and bucket each as `_pending_stub` vs real. Returns
/// `(stubs, real_names)` sets.
pub fn parse_dispatcher(
    dispatcher_path: &Path,
) -> Result<(HashSet<String>, HashSet<String>), DoctorError> {
    if !dispatcher_path.is_file() {
        return Err(DoctorError::NoDispatcher(dispatcher_path.to_path_buf()));
    }
    let body = fs::read_to_string(dispatcher_path).map_err(|source| DoctorError::ReadFile {
        path: dispatcher_path.to_path_buf(),
        source,
    })?;
    let mut stubs = HashSet::new();
    let mut real = HashSet::new();
    let lines: Vec<&str> = body.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("test_") {
            // Format: `test_<name>() { … }` or `test_<name>() {<newline>` …
            if let Some(paren_at) = rest.find('(') {
                let fn_name = format!("test_{}", &rest[..paren_at]);
                // Inspect the body — same-line `_pending_stub` or
                // first body line that's `_pending_stub`.
                let mut is_stub = false;
                if line.contains("_pending_stub") {
                    is_stub = true;
                } else if let Some(next) = lines.get(idx + 1) {
                    if next.trim_start().starts_with("_pending_stub") {
                        is_stub = true;
                    }
                }
                if is_stub {
                    stubs.insert(fn_name);
                } else {
                    real.insert(fn_name);
                }
            }
        }
    }
    Ok((stubs, real))
}

/// Run the criteria audit. Returns the collected `Finding`s — empty
/// vec means clean.
pub fn audit(specs_dir: &Path, dispatcher_path: &Path) -> Result<Vec<Finding>, DoctorError> {
    let annotations = parse_verify_annotations(specs_dir)?;
    let (stubs, real) = parse_dispatcher(dispatcher_path)?;
    let mut findings = Vec::new();
    for ann in &annotations {
        let fn_name = &ann.dispatcher_fn;
        let where_at = format!("{}:{}", ann.spec_file.display(), ann.line_no,);
        if !stubs.contains(fn_name) && !real.contains(fn_name) {
            // Condition 3: missing dispatcher — hard error.
            findings.push(Finding {
                severity: Severity::Error,
                location: where_at,
                message: format!("missing dispatcher `{fn_name}` (referenced in `[verify]`)",),
            });
        } else if ann.checked && stubs.contains(fn_name) {
            // Condition 1: stub + checked box — hard error.
            findings.push(Finding {
                severity: Severity::Error,
                location: where_at,
                message: format!(
                    "criterion is checked `[x]` but dispatcher `{fn_name}` \
                     still calls `_pending_stub`",
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
        let (stubs, real) = parse_dispatcher(&path).expect("parse");
        assert!(stubs.contains("test_one"));
        assert!(stubs.contains("test_three"));
        assert!(real.contains("test_two"));
        assert_eq!(stubs.len() + real.len(), 3);
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
