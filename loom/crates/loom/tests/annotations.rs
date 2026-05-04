//! Bidirectional annotation-integrity gate.
//!
//! Walks `specs/*.md`, regex-extracts every `[verify](path::fn)` or
//! `[judge](path::fn)` annotation, and enforces two contracts:
//!
//! - **Forward direction** — every annotation pointing into
//!   `tests/loom-test.sh` resolves to an existing top-level function in
//!   that file. Annotations pointing to other test runners (e.g.,
//!   `tests/ralph/run-tests.sh`, `tests/judges/*.sh`, `tests/city/*.nix`)
//!   are out of this gate's scope; those runners own their own gates.
//! - **Reverse direction** — every top-level zero-argument `test_*`
//!   function in `tests/loom-test.sh` is referenced by at least one
//!   annotation in some spec. Helper functions (names not starting with
//!   `test_`) are not in scope; the `test_` prefix is the contract for
//!   "this is a verify-runner entry point."
//!
//! **Judge guard**: per spec Annotation Contract rule 2, no `[judge]`
//! annotation pointing into `tests/loom-test.sh` is permitted in v1 —
//! the judge runner doesn't exist yet, so encountering one is a hard
//! error. When a judge runner lands at `tests/judges/loom.sh` (or
//! equivalent), this gate's resolution logic extends to that path.
//!
//! The gate verifies itself: the spec acceptance criterion for it
//! carries `[verify](tests/loom-test.sh::test_acceptance_annotations_resolve)`,
//! whose shell wrapper shells into this very test file.
//!
//! Output on failure: `<spec>:<line>: annotation [verify](path::fn) —
//! function not found` (forward), or `tests/loom-test.sh:<line>: orphan
//! test function `test_X` — not referenced by any annotation in
//! specs/` (reverse). Reviewers can click straight into the offending
//! site.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Test runner this gate validates. Annotations pointing elsewhere
/// belong to a different runner's gate.
const SHELL_RUNNER_PATH: &str = "tests/loom-test.sh";

#[test]
fn acceptance_annotations_resolve() {
    let root = repo_root();
    let runner_path = root.join(SHELL_RUNNER_PATH);
    let runner_body = read(&runner_path);
    let mut violations: Vec<String> = Vec::new();

    for spec_path in spec_files(&root) {
        let body = read(&spec_path);
        let rel_spec = rel(&root, &spec_path);
        for ann in extract_annotations(&body) {
            if ann.path != SHELL_RUNNER_PATH {
                continue;
            }
            if ann.kind == AnnotationKind::Judge {
                violations.push(format!(
                    "{}:{}: annotation [judge]({}::{}) — \
                     [judge] is reserved for the judge runner, which \
                     doesn't exist in v1 (specs/loom-tests.md \
                     §Annotation Contract rule 2)",
                    rel_spec, ann.line, ann.path, ann.fn_name,
                ));
                continue;
            }
            if !shell_function_exists(&runner_body, &ann.fn_name) {
                violations.push(format!(
                    "{}:{}: annotation [verify]({}::{}) — \
                     function not found in {}",
                    rel_spec, ann.line, ann.path, ann.fn_name, SHELL_RUNNER_PATH,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "annotation gate (forward): every [verify]/[judge] in \
         specs/*.md pointing into {} must resolve to an existing \
         function (specs/loom-tests.md §Annotation Integrity Gate). \
         Violations:\n{}",
        SHELL_RUNNER_PATH,
        violations.join("\n"),
    );
}

#[test]
fn no_orphan_test_functions() {
    let root = repo_root();
    let runner_path = root.join(SHELL_RUNNER_PATH);
    let runner_body = read(&runner_path);

    let mut referenced: HashSet<String> = HashSet::new();
    for spec_path in spec_files(&root) {
        let body = read(&spec_path);
        for ann in extract_annotations(&body) {
            if ann.path == SHELL_RUNNER_PATH {
                referenced.insert(ann.fn_name);
            }
        }
    }

    let mut violations: Vec<String> = Vec::new();
    for fn_def in top_level_test_functions(&runner_body) {
        if !referenced.contains(&fn_def.name) {
            violations.push(format!(
                "{}:{}: orphan test function `{}` — not referenced \
                 by any annotation in specs/ (either add a spec \
                 acceptance pointing to it, rename it to match an \
                 existing annotation, or delete it)",
                SHELL_RUNNER_PATH, fn_def.line, fn_def.name,
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "annotation gate (reverse): every top-level `test_*` function \
         in {} must be referenced by at least one annotation in \
         specs/*.md (specs/loom-tests.md §Annotation Integrity Gate). \
         Violations:\n{}",
        SHELL_RUNNER_PATH,
        violations.join("\n"),
    );
}

// ---------------------------------------------------------------------------
// Annotation extraction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationKind {
    Verify,
    Judge,
}

#[derive(Debug)]
struct Annotation {
    kind: AnnotationKind,
    path: String,
    fn_name: String,
    line: usize,
}

/// Walk the markdown body line by line, skipping fenced code blocks, and
/// pull out every `[verify](path::fn)` / `[judge](path::fn)` link.
///
/// The match shape is intentionally narrow:
/// - `[(verify|judge)]` opens the link
/// - `(path::fn)` requires a `::` separator and a Rust-identifier-shaped
///   function name. Annotations using `#` (notify-test.sh#test_foo) or
///   ad-hoc separators (e.g., `tests/city/integration.nix::Wait for X`)
///   come from runners with their own gates and are not in scope.
fn extract_annotations(body: &str) -> Vec<Annotation> {
    let mut out: Vec<Annotation> = Vec::new();
    let mut in_fence = false;
    for (idx, line) in body.lines().enumerate() {
        let lineno = idx + 1;
        if is_code_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_annotations_in_line(line, lineno, &mut out);
    }
    out
}

/// Triple-backtick or triple-tilde fence at the start of a line (after
/// optional indentation). We don't bother distinguishing fence-info
/// strings — the fence itself toggles the in-block state.
fn is_code_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn scan_annotations_in_line(line: &str, lineno: usize, out: &mut Vec<Annotation>) {
    let mut cursor = 0;
    let bytes = line.as_bytes();
    while cursor < bytes.len() {
        let Some(start) = find_subslice(bytes, cursor, b"[") else {
            return;
        };
        cursor = start + 1;
        let Some(close_bracket) = find_subslice(bytes, cursor, b"]") else {
            return;
        };
        let label = &line[start + 1..close_bracket];
        let kind = match label {
            "verify" => AnnotationKind::Verify,
            "judge" => AnnotationKind::Judge,
            _ => continue,
        };
        // Must be immediately followed by `(`.
        let paren_open = close_bracket + 1;
        if paren_open >= bytes.len() || bytes[paren_open] != b'(' {
            continue;
        }
        let Some(paren_close) = find_subslice(bytes, paren_open + 1, b")") else {
            return;
        };
        let inner = &line[paren_open + 1..paren_close];
        cursor = paren_close + 1;
        let Some((path, fn_name)) = split_path_fn(inner) else {
            continue;
        };
        if !is_identifier(fn_name) {
            continue;
        }
        out.push(Annotation {
            kind,
            path: path.to_string(),
            fn_name: fn_name.to_string(),
            line: lineno,
        });
    }
}

fn find_subslice(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// `path::fn` split on the LAST `::`. The path portion may contain
/// further `::` (unlikely for filesystem paths, but cheap to allow).
fn split_path_fn(inner: &str) -> Option<(&str, &str)> {
    let idx = inner.rfind("::")?;
    let path = &inner[..idx];
    let fn_name = &inner[idx + 2..];
    if path.is_empty() || fn_name.is_empty() {
        return None;
    }
    Some((path, fn_name))
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Shell function discovery
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ShellFn {
    name: String,
    line: usize,
}

/// `^test_<ident>()` at column 0, optional whitespace before `{`. Matches
/// only top-level zero-argument function definitions (POSIX `name()`
/// form). The leading `test_` prefix is the contract for "verify-runner
/// entry point" — helper functions use any other name and are exempt.
fn top_level_test_functions(body: &str) -> Vec<ShellFn> {
    let mut out: Vec<ShellFn> = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let lineno = idx + 1;
        if !line.starts_with("test_") {
            continue;
        }
        let Some(name_end) = line.find("()") else {
            continue;
        };
        let name = &line[..name_end];
        if !is_identifier(name) {
            continue;
        }
        let after = &line[name_end + 2..];
        // Tolerate `test_x()` and `test_x() {`. The body must follow on
        // this or a later line; we don't validate the brace location.
        if !after.trim_start().starts_with('{') && !after.trim().is_empty() {
            continue;
        }
        out.push(ShellFn {
            name: name.to_string(),
            line: lineno,
        });
    }
    out
}

/// Forward-direction lookup: does the named function exist as a
/// top-level definition? Cheap line scan rather than a full bash
/// parser — bash function syntax is regular enough that a column-0
/// `name()` match is the contract.
fn shell_function_exists(body: &str, name: &str) -> bool {
    let prefix = format!("{name}()");
    body.lines().any(|line| {
        if !line.starts_with(&prefix) {
            return false;
        }
        let after = &line[prefix.len()..];
        after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || c == '{')
    })
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is .../loom/crates/loom; the repo root is three
    // levels up.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .expect("repo root above loom/crates/loom")
}

fn spec_files(root: &Path) -> Vec<PathBuf> {
    let specs_dir = root.join("specs");
    let mut out: Vec<PathBuf> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&specs_dir) else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        out.push(path);
    }
    out.sort();
    out
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Inline tests for the parser primitives. The integration tests above
// run the gate against the real workspace; these pin parser corner
// cases so a regression in `extract_annotations` shows up locally.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn extracts_canonical_verify() {
        let body = "- [ ] foo\n  [verify](tests/loom-test.sh::test_foo)\n";
        let anns = extract_annotations(body);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].kind, AnnotationKind::Verify);
        assert_eq!(anns[0].path, "tests/loom-test.sh");
        assert_eq!(anns[0].fn_name, "test_foo");
        assert_eq!(anns[0].line, 2);
    }

    #[test]
    fn extracts_judge() {
        let body = "[judge](tests/loom-test.sh::test_bar)\n";
        let anns = extract_annotations(body);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].kind, AnnotationKind::Judge);
        assert_eq!(anns[0].fn_name, "test_bar");
    }

    #[test]
    fn skips_fenced_code() {
        let body = "```\n[verify](tests/loom-test.sh::test_in_fence)\n```\n\
                    [verify](tests/loom-test.sh::test_outside)\n";
        let anns = extract_annotations(body);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].fn_name, "test_outside");
    }

    #[test]
    fn skips_tilde_fenced_code() {
        let body = "~~~\n[verify](tests/loom-test.sh::test_in_fence)\n~~~\n";
        let anns = extract_annotations(body);
        assert!(anns.is_empty());
    }

    #[test]
    fn ignores_hash_separator() {
        // notify-test.sh-style annotations target their own runner.
        let body = "[verify](../tests/notify-test.sh#test_thing)\n";
        let anns = extract_annotations(body);
        assert!(anns.is_empty());
    }

    #[test]
    fn ignores_non_identifier_function() {
        // city integration tests use scenario titles, not idents.
        let body = "[verify](tests/city/integration.nix::Wait for worker)\n";
        let anns = extract_annotations(body);
        assert!(anns.is_empty());
    }

    #[test]
    fn ignores_placeholder() {
        let body = "[verify](...)\n[verify](path#fn)\n";
        let anns = extract_annotations(body);
        assert!(anns.is_empty());
    }

    #[test]
    fn multiple_annotations_per_line() {
        let body = "see [verify](a::test_a) and [verify](b::test_b)\n";
        let anns = extract_annotations(body);
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].fn_name, "test_a");
        assert_eq!(anns[1].fn_name, "test_b");
    }

    #[test]
    fn ignores_other_link_kinds() {
        let body = "[link](http://example.com::not_a_fn)\n";
        let anns = extract_annotations(body);
        assert!(anns.is_empty());
    }

    #[test]
    fn shell_function_exists_matches_top_level() {
        let body = "test_foo() {\n    echo hi\n}\n";
        assert!(shell_function_exists(body, "test_foo"));
    }

    #[test]
    fn shell_function_exists_rejects_indented() {
        let body = "    test_foo() {\n}\n";
        assert!(!shell_function_exists(body, "test_foo"));
    }

    #[test]
    fn shell_function_exists_rejects_substring() {
        let body = "test_foobar() {\n}\n";
        assert!(!shell_function_exists(body, "test_foo"));
    }

    #[test]
    fn top_level_test_functions_finds_definitions() {
        let body = "\
test_one() {
    :
}

# comment
test_two() {
    :
}
helper() {
    :
}
";
        let fns = top_level_test_functions(body);
        let names: Vec<_> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["test_one", "test_two"]);
    }

    #[test]
    fn top_level_test_functions_skips_indented() {
        let body = "    test_indented() {\n}\n";
        let fns = top_level_test_functions(body);
        assert!(fns.is_empty());
    }
}
