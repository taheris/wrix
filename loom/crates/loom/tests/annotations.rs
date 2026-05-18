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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::Attribute;
use syn::visit::Visit;
use walkdir::WalkDir;

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

/// Verifies dispatchers in `tests/loom-test.sh` invoke cargo tests whose
/// names actually resolve. `cargo test ... -- <name>` exits 0 silently when
/// `<name>` matches nothing, so a stale rename or a typo lets a
/// dispatcher claim coverage without exercising any code.
///
/// For each top-level `test_*` dispatcher: parse the body, extract cargo
/// invocations (direct `cargo_run test ...` lines and helper calls like
/// `lock_cargo_test foo` whose definitions are themselves cargo wrappers),
/// then verify each test-name argument matches at least one `#[test]` /
/// `#[tokio::test]` function defined in the corresponding cargo target.
#[test]
fn dispatcher_cargo_tests_resolve() {
    let root = repo_root();
    let runner_path = root.join(SHELL_RUNNER_PATH);
    let runner_body = read(&runner_path);
    let helpers = parse_dispatch_helpers(&runner_body);

    let mut violations: Vec<String> = Vec::new();
    let mut name_cache: HashMap<(String, TestTarget), TargetIndex> = HashMap::new();

    for dispatcher in collect_dispatchers(&runner_body) {
        for invocation in extract_cargo_invocations(&dispatcher.body, &helpers) {
            let key = (invocation.crate_name.clone(), invocation.target.clone());
            let index = name_cache
                .entry(key.clone())
                .or_insert_with(|| collect_test_names(&root, &key.0, &key.1));
            for test_name in &invocation.test_names {
                if !cargo_name_resolves(index, test_name) {
                    violations.push(format!(
                        "{}:{}: dispatcher `{}` invokes cargo test \
                         `{}` in -p {} {} which does not match any \
                         defined #[test] / #[tokio::test] function \
                         (`cargo test ... -- <name>` exits 0 silently \
                         when the name resolves to nothing, so this \
                         dispatcher claims coverage without exercising \
                         any code)",
                        SHELL_RUNNER_PATH,
                        dispatcher.line,
                        dispatcher.name,
                        test_name,
                        invocation.crate_name,
                        invocation.target.fmt_arg(),
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "annotation gate (cargo resolution): every cargo test name \
         invoked by a dispatcher in {} must resolve to a defined \
         #[test] function in the named target (specs/loom-tests.md \
         §Annotation Integrity Gate). Violations:\n{}",
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
/// A trailing space-prefixed `@unit-ok` marker is stripped off — that's
/// the doctor opt-out syntax, not part of the function name.
fn split_path_fn(inner: &str) -> Option<(&str, &str)> {
    let idx = inner.rfind("::")?;
    let path = &inner[..idx];
    let raw_fn = &inner[idx + 2..];
    let fn_name = match raw_fn.split_once(" @unit-ok") {
        Some((name, _)) => name.trim_end(),
        None => raw_fn,
    };
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
    // Two layouts to handle:
    //   - dev tree: `repo/loom/crates/loom/` is the manifest, `specs/` and
    //     `tests/loom-test.sh` live under `repo/` (three levels up).
    //   - nix sandbox: `src = ../../loom` so the build dir IS the loom
    //     workspace root; `tests/loom/default.nix` stages `specs/` and
    //     `tests/loom-test.sh` under that build dir (two levels up).
    // Walk ancestors and pick the closest one that carries both
    // anchors, so neither layout needs a sandbox-only env var.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        if ancestor.join("specs").is_dir() && ancestor.join(SHELL_RUNNER_PATH).is_file() {
            return ancestor.to_path_buf();
        }
    }
    panic!(
        "could not locate repo root above {} — neither dev-tree nor \
         nix-sandbox layout matched. Stage `specs/` and `{}` next to \
         the loom workspace under the build dir.",
        manifest_dir.display(),
        SHELL_RUNNER_PATH,
    );
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
// Cargo invocation extraction & resolution.
//
// `tests/loom-test.sh` dispatches each `test_X` to one or more `cargo test`
// invocations — sometimes inline (`cargo_run test -p X --test Y -- name1 …`),
// sometimes via a helper (`lock_cargo_test name`), where the helper's own
// body is `cargo_run test -p loom-driver --test lock_manager "$1" -- ...`.
// We parse the helper definitions out of the script so the gate stays
// resilient when new helpers are introduced.
//
// The matching rule against defined `#[test]` functions is intentionally
// substring-based in both directions: cargo's default filter is substring
// against the fully-qualified path, so a leaf name like `foo` resolves
// against a fn `module::tests::foo`, and a fully-qualified arg like
// `module::tests::foo` resolves against the leaf fn. Adding `--exact` only
// narrows the match — for this gate's purpose (catch non-existent names),
// substring is the sufficient lower-bound: a name that doesn't substring-
// match anything cannot possibly exact-match either.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestTarget {
    Lib,
    Integration(String),
    /// No `--lib` / `--test` specified — cargo runs every test target in
    /// the crate (lib + each integration binary) and substring-matches the
    /// filter against the union of their FQ paths.
    AllTargets,
}

impl TestTarget {
    fn fmt_arg(&self) -> String {
        match self {
            TestTarget::Lib => "--lib".into(),
            TestTarget::Integration(name) => format!("--test {name}"),
            TestTarget::AllTargets => "(all targets)".into(),
        }
    }
}

#[derive(Debug)]
struct DispatchHelper {
    crate_name: String,
    target: TestTarget,
    /// Literal prefix prepended to the helper's `$1` argument.
    /// E.g., for `scratch_cargo_test foo` invoking
    /// `cargo_run test -p loom-driver --lib "scratch::tests::$1" ...`,
    /// the prefix is `scratch::tests::`.
    prefix: String,
}

#[derive(Debug)]
struct Dispatcher {
    name: String,
    line: usize,
    body: String,
}

#[derive(Debug)]
struct CargoInvocation {
    crate_name: String,
    target: TestTarget,
    test_names: Vec<String>,
}

#[derive(Debug)]
struct ShellFunc {
    name: String,
    line: usize,
    body: String,
}

/// Walk the shell script, find every top-level function definition, and
/// return its name, header line number, and body (text between the
/// outermost braces). Tracks brace depth and skips quoted regions so a
/// `{` inside a string doesn't open a phantom block. Heredocs in this
/// repo's runner don't carry unbalanced braces, so we ignore them.
fn top_level_functions(body: &str) -> Vec<ShellFunc> {
    let mut out: Vec<ShellFunc> = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        let lineno = idx + 1;
        if let Some((name, rest)) = parse_function_header(line) {
            // Find the opening `{` (may be on header line or a later line),
            // then scan to the matching `}` tracking brace depth.
            let mut depth = 0i32;
            let mut body_text = String::new();
            let mut started = false;
            let mut cursor_line = idx;
            let mut cursor_input: &str = rest;
            'outer: loop {
                let mut buf = String::new();
                let mut in_single = false;
                let mut in_double = false;
                let mut in_backtick = false;
                let mut comment = false;
                // Tracks whether the previous emitted char was whitespace
                // (or we're at the start of input). Used to disambiguate
                // bash comments (`#` after whitespace) from parameter
                // expansion (`${var#pat}`, `${#var}`) where `#` is not a
                // comment start.
                let mut prev_was_ws = true;
                for c in cursor_input.chars() {
                    if comment {
                        buf.push(c);
                        prev_was_ws = false;
                        continue;
                    }
                    if !in_double && !in_backtick && c == '\'' {
                        in_single = !in_single;
                        buf.push(c);
                        prev_was_ws = false;
                        continue;
                    }
                    if !in_single && !in_backtick && c == '"' {
                        in_double = !in_double;
                        buf.push(c);
                        prev_was_ws = false;
                        continue;
                    }
                    if !in_single && !in_double && c == '`' {
                        in_backtick = !in_backtick;
                        buf.push(c);
                        prev_was_ws = false;
                        continue;
                    }
                    if !in_single && !in_double && !in_backtick && c == '#' && prev_was_ws {
                        // Bash comment — rest of the line is text, not
                        // shell tokens. Skip brace-balancing on the tail.
                        comment = true;
                        buf.push(c);
                        continue;
                    }
                    prev_was_ws = c.is_whitespace();
                    if in_single || in_double || in_backtick {
                        buf.push(c);
                        continue;
                    }
                    if c == '{' {
                        depth += 1;
                        if !started {
                            started = true;
                            // Don't include the opening `{` in body_text.
                            buf.clear();
                            continue;
                        }
                        buf.push(c);
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            body_text.push_str(&buf);
                            break 'outer;
                        }
                        buf.push(c);
                    } else {
                        buf.push(c);
                    }
                }
                if started {
                    body_text.push_str(&buf);
                    body_text.push('\n');
                }
                cursor_line += 1;
                if cursor_line >= lines.len() {
                    break;
                }
                cursor_input = lines[cursor_line];
            }
            if started {
                out.push(ShellFunc {
                    name: name.to_string(),
                    line: lineno,
                    body: body_text,
                });
                idx = cursor_line + 1;
                continue;
            }
        }
        idx += 1;
    }
    out
}

/// Match `<name>()` at column 0 — same shape as `top_level_test_functions`
/// uses for the reverse gate, but returns the remainder of the line so the
/// caller can continue scanning into the function body.
fn parse_function_header(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() || line.starts_with(char::is_whitespace) {
        return None;
    }
    let name_end = line.find("()")?;
    let name = &line[..name_end];
    if !is_identifier(name) {
        return None;
    }
    let after = &line[name_end + 2..];
    let trimmed = after.trim_start();
    if !trimmed.is_empty() && !trimmed.starts_with('{') {
        return None;
    }
    Some((name, after))
}

/// Find dispatch helpers: functions whose body is a single `cargo_run test`
/// statement that takes a `$1` argument. The helper's `(crate, target,
/// prefix)` lets us expand its callers.
fn parse_dispatch_helpers(body: &str) -> HashMap<String, DispatchHelper> {
    let mut out: HashMap<String, DispatchHelper> = HashMap::new();
    for func in top_level_functions(body) {
        if func.name.starts_with("test_") {
            continue;
        }
        let stmts = bash_statements(&func.body);
        if stmts.len() != 1 {
            continue;
        }
        let tokens = tokenize_bash(&stmts[0]);
        let Some(invocation) = parse_cargo_invocation_tokens(&tokens) else {
            continue;
        };
        // The helper substitutes `$1` somewhere in the test-name position.
        // Identify the token that contains `$1` and extract the literal
        // prefix before it.
        let mut prefix: Option<String> = None;
        for name in &invocation.test_names {
            if let Some(at) = name.find("$1") {
                prefix = Some(name[..at].to_string());
                break;
            }
        }
        let Some(prefix) = prefix else {
            continue;
        };
        out.insert(
            func.name.clone(),
            DispatchHelper {
                crate_name: invocation.crate_name,
                target: invocation.target,
                prefix,
            },
        );
    }
    out
}

/// Top-level `test_*` shell functions plus their bodies.
fn collect_dispatchers(body: &str) -> Vec<Dispatcher> {
    top_level_functions(body)
        .into_iter()
        .filter(|f| f.name.starts_with("test_"))
        .map(|f| Dispatcher {
            name: f.name,
            line: f.line,
            body: f.body,
        })
        .collect()
}

/// Walk a dispatcher body and emit every cargo invocation it produces.
/// Handles both direct `cargo_run test ...` lines and helper calls.
fn extract_cargo_invocations(
    body: &str,
    helpers: &HashMap<String, DispatchHelper>,
) -> Vec<CargoInvocation> {
    let mut out: Vec<CargoInvocation> = Vec::new();
    for stmt in bash_statements(body) {
        let tokens = tokenize_bash(&stmt);
        if tokens.is_empty() {
            continue;
        }
        // Skip stub markers and comments handled by the tokenizer.
        if let Some(invocation) = parse_cargo_invocation_tokens(&tokens) {
            out.push(invocation);
            continue;
        }
        if let Some(helper) = helpers.get(tokens[0].as_str()) {
            // Helper invocation. Each subsequent positional arg becomes a
            // test-name argument (prefix-substituted).
            let mut names = Vec::new();
            for arg in tokens.iter().skip(1) {
                if arg.starts_with('-') {
                    continue;
                }
                names.push(format!("{}{}", helper.prefix, arg));
            }
            if !names.is_empty() {
                out.push(CargoInvocation {
                    crate_name: helper.crate_name.clone(),
                    target: helper.target.clone(),
                    test_names: names,
                });
            }
        }
    }
    out
}

/// Recognized cargo flags that consume the following token as a value.
const CARGO_FLAGS_WITH_VALUE: &[&str] = &[
    "-p",
    "--package",
    "--test",
    "--bin",
    "--example",
    "--bench",
    "--features",
    "--target",
    "--target-dir",
    "--manifest-path",
    "--message-format",
    "--profile",
    "--config",
    "--color",
];

/// Recognized libtest runner flags (after `--`) that consume the following
/// token as a value. Anything else after `--` that doesn't start with `-`
/// is a positional test-name filter.
const LIBTEST_FLAGS_WITH_VALUE: &[&str] =
    &["--test-threads", "--skip", "--logfile", "--format", "-Z"];

fn parse_cargo_invocation_tokens(tokens: &[String]) -> Option<CargoInvocation> {
    if tokens.is_empty() {
        return None;
    }
    // Accept either `cargo_run test ...` or `cargo test ...`.
    let mut i = 0;
    match tokens[i].as_str() {
        "cargo_run" => {
            i += 1;
            if i >= tokens.len() || tokens[i] != "test" {
                return None;
            }
            i += 1;
        }
        "cargo" => {
            i += 1;
            if i >= tokens.len() || tokens[i] != "test" {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    let mut crate_name: Option<String> = None;
    let mut target: Option<TestTarget> = None;
    let mut cargo_positionals: Vec<String> = Vec::new();
    // Phase 1: cargo args before `--`.
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "--" {
            i += 1;
            break;
        }
        if tok == "--lib" {
            target = Some(TestTarget::Lib);
            i += 1;
            continue;
        }
        if let Some(rest) = tok.strip_prefix("--test=") {
            target = Some(TestTarget::Integration(rest.to_string()));
            i += 1;
            continue;
        }
        if tok == "--test" {
            if let Some(name) = tokens.get(i + 1) {
                target = Some(TestTarget::Integration(name.clone()));
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(rest) = tok
            .strip_prefix("-p=")
            .or_else(|| tok.strip_prefix("--package="))
        {
            crate_name = Some(rest.to_string());
            i += 1;
            continue;
        }
        if tok == "-p" || tok == "--package" {
            if let Some(name) = tokens.get(i + 1) {
                crate_name = Some(name.clone());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(eq_idx) = tok.find('=')
            && CARGO_FLAGS_WITH_VALUE.contains(&&tok[..eq_idx])
        {
            i += 1;
            continue;
        }
        if CARGO_FLAGS_WITH_VALUE.contains(&tok.as_str()) {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        cargo_positionals.push(tok.clone());
        i += 1;
    }
    // Phase 2: libtest args after `--`.
    let mut runner_positionals: Vec<String> = Vec::new();
    while i < tokens.len() {
        let tok = &tokens[i];
        if let Some(eq_idx) = tok.find('=')
            && LIBTEST_FLAGS_WITH_VALUE.contains(&&tok[..eq_idx])
        {
            i += 1;
            continue;
        }
        if LIBTEST_FLAGS_WITH_VALUE.contains(&tok.as_str()) {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        runner_positionals.push(tok.clone());
        i += 1;
    }
    let crate_name = crate_name?;
    let target = target.unwrap_or(TestTarget::AllTargets);
    let mut test_names = cargo_positionals;
    test_names.extend(runner_positionals);
    if test_names.is_empty() {
        return None;
    }
    Some(CargoInvocation {
        crate_name,
        target,
        test_names,
    })
}

/// Split a bash body into individual statements. Joins backslash-newline
/// continuations, splits on `\n` and `;`, and drops blank/comment lines.
fn bash_statements(body: &str) -> Vec<String> {
    let mut joined = String::new();
    let mut prev_continuation = false;
    for line in body.lines() {
        let trimmed = line.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            joined.push_str(stripped);
            joined.push(' ');
            prev_continuation = true;
        } else {
            joined.push_str(trimmed);
            joined.push('\n');
            prev_continuation = false;
        }
    }
    let _ = prev_continuation;
    let mut out: Vec<String> = Vec::new();
    for piece in joined.split(['\n', ';']) {
        let trimmed = piece.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

/// Split a bash statement into tokens, honoring single- and double-quoted
/// strings (the quote characters are stripped, the content is preserved).
/// Doesn't perform variable expansion — `"$1"` becomes the token `$1`.
fn tokenize_bash(stmt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;
    let mut chars = stmt.chars().peekable();
    while let Some(c) = chars.next() {
        if !in_double && c == '\'' {
            in_single = !in_single;
            has_token = true;
            continue;
        }
        if !in_single && c == '"' {
            in_double = !in_double;
            has_token = true;
            continue;
        }
        if !in_single && !in_double && c.is_whitespace() {
            if has_token {
                out.push(std::mem::take(&mut current));
                has_token = false;
            }
            continue;
        }
        if !in_single
            && c == '\\'
            && let Some(&next) = chars.peek()
        {
            current.push(next);
            chars.next();
            has_token = true;
            continue;
        }
        current.push(c);
        has_token = true;
    }
    if has_token {
        out.push(current);
    }
    out
}

/// Collect fully-qualified test paths from the chosen cargo target.
/// Cargo's default filter is substring match against fully-qualified
/// paths; reconstructing those paths (file-derived module path + nested
/// `mod` blocks + fn name) lets us match cargo's behavior precisely.
///
/// We combine a syn AST walk (handles direct `#[test] fn x() {}` cases
/// at any depth of `mod foo { … }` nesting) with a text scan that picks
/// up macro-wrapped tests (`proptest! { #[test] fn foo(…) }`) where syn
/// sees an opaque `Item::Macro` and never descends.
#[derive(Default)]
struct TargetIndex {
    /// Every defined test's fully-qualified path, e.g.
    /// `bd::label::tests::serde_round_trips_as_plain_string` or, for an
    /// integration-test target, `tests::ensure_widget`.
    fq_paths: HashSet<String>,
}

fn collect_test_names(root: &Path, crate_name: &str, target: &TestTarget) -> TargetIndex {
    let crate_dir = crate_dir(root, crate_name);
    let mut index = TargetIndex::default();
    match target {
        TestTarget::Lib => collect_lib(&crate_dir, &mut index),
        TestTarget::Integration(name) => {
            let path = crate_dir.join("tests").join(format!("{name}.rs"));
            if path.is_file() {
                // Integration-test binaries are their own crate root; the
                // top-level fn `x` has FQ path `x`, not `<name>::x`.
                scan_file(&path, "", &mut index);
            }
        }
        TestTarget::AllTargets => {
            collect_lib(&crate_dir, &mut index);
            let tests_dir = crate_dir.join("tests");
            if tests_dir.is_dir() {
                for entry in std::fs::read_dir(&tests_dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let p = entry.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                        scan_file(&p, "", &mut index);
                    }
                }
            }
        }
    }
    index
}

/// Resolve a crate's source directory across both layouts `repo_root`
/// can return: the dev tree (`<repo>/loom/crates/<name>`) and the nix
/// sandbox where the loom workspace IS the root (`<root>/crates/<name>`).
fn crate_dir(root: &Path, crate_name: &str) -> PathBuf {
    let dev = root.join("loom").join("crates").join(crate_name);
    if dev.is_dir() {
        return dev;
    }
    root.join("crates").join(crate_name)
}

fn collect_lib(crate_dir: &Path, index: &mut TargetIndex) {
    let src_dir = crate_dir.join("src");
    if !src_dir.is_dir() {
        return;
    }
    for entry in WalkDir::new(&src_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let module_path = lib_file_module_path(&src_dir, p);
        scan_file(p, &module_path, index);
    }
}

/// Convert a source file path into its module path. `src/lib.rs` and
/// `src/main.rs` are the crate root (empty). `src/foo.rs` and
/// `src/foo/mod.rs` are `foo`. `src/foo/bar.rs` is `foo::bar`.
fn lib_file_module_path(src_dir: &Path, file: &Path) -> String {
    let Ok(rel) = file.strip_prefix(src_dir) else {
        return String::new();
    };
    let file_name = rel.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    let parent = rel.parent();
    let parent_segments: Vec<String> = parent
        .into_iter()
        .flat_map(|p| p.iter())
        .filter_map(|c| c.to_str().map(str::to_string))
        .filter(|s| !s.is_empty())
        .collect();
    let leaf = file_name.strip_suffix(".rs").unwrap_or(file_name);
    let mut segments = parent_segments;
    match leaf {
        "lib" | "main" | "mod" => {}
        _ => segments.push(leaf.to_string()),
    }
    segments.join("::")
}

fn scan_file(path: &Path, file_module_path: &str, index: &mut TargetIndex) {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    // Syn walk: catches every `#[test] fn x()` whose enclosing context is
    // valid Rust syntax. Tracks the module stack so the FQ path comes out
    // right for tests nested inside `mod foo { … }`.
    if let Ok(file) = syn::parse_file(&body) {
        let mut visitor = TestFnVisitor {
            file_module_path,
            module_stack: Vec::new(),
            out: &mut index.fq_paths,
        };
        visitor.visit_file(&file);
    }
    // Text scan: catches definitions inside macro bodies (proptest!, etc.)
    // where syn sees Item::Macro and never descends. Module tracking is
    // brace-depth based; good enough since macro bodies don't fork into
    // other Rust items here.
    scan_text_for_tests(&body, file_module_path, index);
}

struct TestFnVisitor<'a> {
    file_module_path: &'a str,
    module_stack: Vec<String>,
    out: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for TestFnVisitor<'a> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if attrs_contain_test_marker(&node.attrs) {
            self.out.insert(join_path(
                self.file_module_path,
                &self.module_stack,
                &node.sig.ident.to_string(),
            ));
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.module_stack.push(node.ident.to_string());
        syn::visit::visit_item_mod(self, node);
        self.module_stack.pop();
    }
}

fn join_path(file_path: &str, mods: &[String], leaf: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !file_path.is_empty() {
        parts.push(file_path);
    }
    for m in mods {
        parts.push(m.as_str());
    }
    parts.push(leaf);
    parts.join("::")
}

fn attrs_contain_test_marker(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        let path = attr.path();
        if let Some(seg) = path.segments.last()
            && seg.ident == "test"
        {
            return true;
        }
    }
    false
}

/// Line-based scan to catch test fns inside macros. Tracks brace depth +
/// a stack of `mod <name> { … }` entries so the FQ path is reconstructed
/// the same way syn would have produced it. The `pending_test` flag is
/// reset on any non-attribute / non-comment line.
fn scan_text_for_tests(body: &str, file_module_path: &str, index: &mut TargetIndex) {
    let mut module_stack: Vec<(String, i32)> = Vec::new(); // (name, depth-at-entry)
    let mut depth: i32 = 0;
    let mut pending_test = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if is_test_attribute_line(trimmed) {
            pending_test = true;
            update_brace_depth(trimmed, &mut depth);
            continue;
        }
        if let Some(mod_name) = parse_mod_name(trimmed) {
            // `mod foo;` is a declaration without body; depth unchanged.
            // `mod foo { … }` opens a new scope.
            if trimmed.contains('{') {
                module_stack.push((mod_name, depth));
            }
            update_brace_depth(trimmed, &mut depth);
            // Close any modules whose scope ended on this line.
            while module_stack
                .last()
                .map(|(_, d)| depth <= *d)
                .unwrap_or(false)
            {
                module_stack.pop();
            }
            pending_test = false;
            continue;
        }
        if let Some(name) = parse_fn_name(trimmed) {
            if pending_test {
                let mods: Vec<String> = module_stack.iter().map(|(n, _)| n.clone()).collect();
                index
                    .fq_paths
                    .insert(join_path(file_module_path, &mods, &name));
            }
            pending_test = false;
            update_brace_depth(trimmed, &mut depth);
            continue;
        }
        update_brace_depth(trimmed, &mut depth);
        while module_stack
            .last()
            .map(|(_, d)| depth <= *d)
            .unwrap_or(false)
        {
            module_stack.pop();
        }
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            pending_test = false;
        }
    }
}

fn update_brace_depth(line: &str, depth: &mut i32) {
    let mut in_single = false;
    let mut in_double = false;
    let mut iter = line.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\\' {
            iter.next();
            continue;
        }
        if !in_double && c == '\'' {
            in_single = !in_single;
            continue;
        }
        if !in_single && c == '"' {
            in_double = !in_double;
            continue;
        }
        if in_single || in_double {
            continue;
        }
        if c == '{' {
            *depth += 1;
        } else if c == '}' {
            *depth -= 1;
        }
    }
}

fn is_test_attribute_line(line: &str) -> bool {
    // Strip outer `#[` ... `]`.
    let Some(rest) = line.strip_prefix("#[") else {
        return false;
    };
    let Some(inner) = rest.strip_suffix(']') else {
        return false;
    };
    // Drop any argument list — `#[test(...)]` or `#[tokio::test(flavor = ...)]`.
    let path = inner.split('(').next().unwrap_or(inner).trim();
    // The attribute's last `::` segment carries the runner name.
    let last = path.rsplit("::").next().unwrap_or(path);
    last == "test"
}

fn parse_mod_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub(crate) mod ")
        .or_else(|| line.strip_prefix("pub mod "))
        .or_else(|| line.strip_prefix("mod "))?;
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if is_identifier(name) {
        Some(name.to_string())
    } else {
        None
    }
}

fn parse_fn_name(line: &str) -> Option<String> {
    // Strip visibility / `async` / function-attribute prefixes greedily.
    let mut rest = line;
    for prefix in ["pub(crate) ", "pub ", "async "] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
        }
    }
    let rest = rest.strip_prefix("fn ")?;
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if is_identifier(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Cargo's default filter is substring match against fully-qualified test
/// paths: the filter `foo::tests` matches every test whose FQ path
/// contains `foo::tests` as substring. We replicate that by checking the
/// arg against the precomputed set of FQ paths for the target.
fn cargo_name_resolves(index: &TargetIndex, cargo_arg: &str) -> bool {
    index.fq_paths.iter().any(|fq| fq.contains(cargo_arg))
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

    // -----------------------------------------------------------------
    // Cargo invocation extraction
    // -----------------------------------------------------------------

    #[test]
    fn parse_cargo_invocation_simple_after_dashdash() {
        let toks = tokenize_bash("cargo_run test -p loom --test style --quiet -- no_thread_sleep");
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.crate_name, "loom");
        assert_eq!(inv.target, TestTarget::Integration("style".into()));
        assert_eq!(inv.test_names, vec!["no_thread_sleep".to_string()]);
    }

    #[test]
    fn parse_cargo_invocation_multiple_names_after_dashdash() {
        let toks = tokenize_bash(
            "cargo_run test -p loom-templates --test render --quiet -- alpha beta gamma",
        );
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.target, TestTarget::Integration("render".into()));
        assert_eq!(inv.test_names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn parse_cargo_invocation_positional_before_dashdash() {
        let toks = tokenize_bash(
            "cargo_run test -p loom-driver --test state_db foo -- --exact --nocapture --quiet",
        );
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.crate_name, "loom-driver");
        assert_eq!(inv.target, TestTarget::Integration("state_db".into()));
        assert_eq!(inv.test_names, vec!["foo".to_string()]);
    }

    #[test]
    fn parse_cargo_invocation_lib_target() {
        let toks = tokenize_bash(
            "cargo_run test -p loom-workflow --lib --quiet -- run::spawn::tests::spawn_config_env_includes_loom_inside_marker",
        );
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.crate_name, "loom-workflow");
        assert_eq!(inv.target, TestTarget::Lib);
        assert_eq!(inv.test_names.len(), 1);
        assert!(inv.test_names[0].ends_with("spawn_config_env_includes_loom_inside_marker"));
    }

    #[test]
    fn parse_cargo_invocation_libtest_test_threads_consumes_value() {
        let toks = tokenize_bash(
            "cargo_run test -p loom --test marker_gate -- --test-threads=1 only_test",
        );
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.test_names, vec!["only_test".to_string()]);
    }

    #[test]
    fn parse_cargo_invocation_skips_unknown_flags() {
        let toks = tokenize_bash(
            "cargo_run test -p loom --test x --quiet --no-fail-fast -- --unexpected name1",
        );
        let inv = parse_cargo_invocation_tokens(&toks).expect("invocation parses");
        assert_eq!(inv.test_names, vec!["name1".to_string()]);
    }

    #[test]
    fn parse_dispatch_helpers_recognizes_lib_wrapper() {
        let body = "lock_cargo_test() {\n    cargo_run test -p loom-driver --test lock_manager \"$1\" -- --exact --nocapture --quiet\n}\n";
        let helpers = parse_dispatch_helpers(body);
        let helper = helpers.get("lock_cargo_test").expect("helper parsed");
        assert_eq!(helper.crate_name, "loom-driver");
        assert_eq!(
            helper.target,
            TestTarget::Integration("lock_manager".into())
        );
        assert_eq!(helper.prefix, "");
    }

    #[test]
    fn parse_dispatch_helpers_recognizes_prefix_wrapper() {
        let body = "scratch_cargo_test() {\n    cargo_run test -p loom-driver --lib \"scratch::tests::$1\" -- --exact --nocapture --quiet\n}\n";
        let helpers = parse_dispatch_helpers(body);
        let helper = helpers.get("scratch_cargo_test").expect("helper parsed");
        assert_eq!(helper.crate_name, "loom-driver");
        assert_eq!(helper.target, TestTarget::Lib);
        assert_eq!(helper.prefix, "scratch::tests::");
    }

    #[test]
    fn parse_dispatch_helpers_ignores_multi_statement_bodies() {
        let body = "complex_helper() {\n    echo hi\n    cargo_run test -p loom --test x \"$1\" -- --exact\n}\n";
        let helpers = parse_dispatch_helpers(body);
        assert!(!helpers.contains_key("complex_helper"));
    }

    #[test]
    fn extract_cargo_invocations_expands_helpers() {
        // Helpers live in the runner; the dispatcher body invokes them.
        let runner = "lock_cargo_test() {\n    cargo_run test -p loom-driver --test lock_manager \"$1\" -- --exact --nocapture --quiet\n}\n";
        let helpers = parse_dispatch_helpers(runner);
        let body = "lock_cargo_test second_acquire_times_out_with_spec_busy\nlock_cargo_test crash_releases_spec_lock\n";
        let invocations = extract_cargo_invocations(body, &helpers);
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].crate_name, "loom-driver");
        assert_eq!(
            invocations[0].target,
            TestTarget::Integration("lock_manager".into())
        );
        assert_eq!(
            invocations[0].test_names,
            vec!["second_acquire_times_out_with_spec_busy".to_string()],
        );
        assert_eq!(
            invocations[1].test_names,
            vec!["crash_releases_spec_lock".to_string()],
        );
    }

    #[test]
    fn extract_cargo_invocations_applies_prefix() {
        let runner = "scratch_cargo_test() {\n    cargo_run test -p loom-driver --lib \"scratch::tests::$1\" -- --exact --nocapture --quiet\n}\n";
        let helpers = parse_dispatch_helpers(runner);
        let body = "scratch_cargo_test open_creates_layout_and_drop_removes_it\n";
        let invocations = extract_cargo_invocations(body, &helpers);
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].test_names,
            vec!["scratch::tests::open_creates_layout_and_drop_removes_it".to_string()],
        );
    }

    #[test]
    fn extract_cargo_invocations_joins_backslash_continuations() {
        let helpers = HashMap::new();
        let body = "cargo_run test -p loom-templates --test render --quiet -- \\\n    name_one \\\n    name_two\n";
        let invocations = extract_cargo_invocations(body, &helpers);
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].test_names,
            vec!["name_one".to_string(), "name_two".to_string()],
        );
    }

    // -----------------------------------------------------------------
    // Resolution / regression
    // -----------------------------------------------------------------

    fn index_with_fqs(fqs: &[&str]) -> TargetIndex {
        let mut index = TargetIndex::default();
        for f in fqs {
            index.fq_paths.insert((*f).to_string());
        }
        index
    }

    #[test]
    fn cargo_name_resolves_leaf_match() {
        let index = index_with_fqs(&["render::tests::review_renders_review_context_fields"]);
        assert!(cargo_name_resolves(
            &index,
            "review_renders_review_context_fields"
        ));
        assert!(cargo_name_resolves(
            &index,
            "render::tests::review_renders_review_context_fields",
        ));
    }

    #[test]
    fn cargo_name_resolves_rejects_stale_rename() {
        // Stale dispatcher name pointing at a renamed target.
        let index = index_with_fqs(&["render::tests::review_renders_review_context_fields"]);
        assert!(!cargo_name_resolves(
            &index,
            "check_renders_review_context_fields",
        ));
    }

    #[test]
    fn cargo_name_resolves_substring_widening_match() {
        // cargo's default filter is substring; a leaf that's contained in a
        // fully-qualified name is a valid resolution.
        let index = index_with_fqs(&["mod_a::tests::does_something_specific"]);
        assert!(cargo_name_resolves(&index, "does_something"));
    }

    #[test]
    fn cargo_name_resolves_module_prefix_filter() {
        // `in_place::tests` is a module-path-only filter; cargo would match
        // it against every test under that module.
        let index = index_with_fqs(&[
            "in_place::tests::first_tick_writes_text_without_escape",
            "in_place::tests::second_tick_overwrites",
        ]);
        assert!(cargo_name_resolves(&index, "in_place::tests"));
    }

    #[test]
    fn cargo_name_resolves_rejects_wrong_module_prefix() {
        // Tightened gate: a qualified arg whose module prefix doesn't
        // match any defined FQ path is rejected, even if the leaf fn name
        // exists elsewhere in the crate.
        let index = index_with_fqs(&["bd::label::tests::serde_round_trips_as_plain_string"]);
        assert!(!cargo_name_resolves(
            &index,
            "identifier::bead::tests::serde_round_trips_as_plain_string",
        ));
    }

    #[test]
    fn dispatcher_with_broken_cargo_name_is_flagged() {
        let runner = "\
state_db_cargo_test() {
    cargo_run test -p loom-driver --test state_db \"$1\" -- --exact --nocapture --quiet
}

test_real_thing() {
    state_db_cargo_test state_db_init_creates_tables
}

test_stale_rename() {
    state_db_cargo_test does_not_exist_anymore
}
";
        let helpers = parse_dispatch_helpers(runner);
        let dispatchers = collect_dispatchers(runner);
        let index = index_with_fqs(&["state_db_init_creates_tables"]);

        let mut violations = Vec::new();
        for disp in &dispatchers {
            for inv in extract_cargo_invocations(&disp.body, &helpers) {
                for name in &inv.test_names {
                    if !cargo_name_resolves(&index, name) {
                        violations.push((disp.name.clone(), name.clone()));
                    }
                }
            }
        }
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].0, "test_stale_rename");
        assert_eq!(violations[0].1, "does_not_exist_anymore");
    }

    #[test]
    fn scan_text_captures_proptest_wrapped_fns() {
        // proptest! wraps its body in a macro so syn sees Item::Macro. The
        // text scanner has to pick up the inner #[test] fn defs anyway.
        let body = "\
proptest! {
    #[test]
    fn jsonl_arbitrary_bytes_never_panic(input in \".{0,512}\") {
        let _ = noop(input);
    }
}
";
        let mut index = TargetIndex::default();
        scan_text_for_tests(body, "", &mut index);
        assert!(index.fq_paths.contains("jsonl_arbitrary_bytes_never_panic"));
    }

    #[test]
    fn scan_text_tracks_module_nesting() {
        let body = "\
mod outer {
    mod inner {
        #[test]
        fn nested() {}
    }
}
";
        let mut index = TargetIndex::default();
        scan_text_for_tests(body, "file_mod", &mut index);
        assert!(
            index.fq_paths.contains("file_mod::outer::inner::nested"),
            "got: {:?}",
            index.fq_paths,
        );
    }

    #[test]
    fn lib_file_module_path_for_canonical_layouts() {
        use std::path::PathBuf;
        let src = PathBuf::from("/r/src");
        assert_eq!(lib_file_module_path(&src, &src.join("lib.rs")), "");
        assert_eq!(lib_file_module_path(&src, &src.join("main.rs")), "");
        assert_eq!(lib_file_module_path(&src, &src.join("foo.rs")), "foo");
        assert_eq!(lib_file_module_path(&src, &src.join("foo/mod.rs")), "foo");
        assert_eq!(
            lib_file_module_path(&src, &src.join("foo/bar.rs")),
            "foo::bar"
        );
        assert_eq!(
            lib_file_module_path(&src, &src.join("foo/bar/baz.rs")),
            "foo::bar::baz",
        );
    }

    #[test]
    fn is_test_attribute_line_accepts_known_runners() {
        assert!(is_test_attribute_line("#[test]"));
        assert!(is_test_attribute_line("#[tokio::test]"));
        assert!(is_test_attribute_line(
            "#[tokio::test(flavor = \"current_thread\")]"
        ));
        assert!(is_test_attribute_line("#[some::other::test]"));
        assert!(!is_test_attribute_line("#[derive(Debug)]"));
        assert!(!is_test_attribute_line("#[cfg(test)]"));
    }

    #[test]
    fn top_level_functions_returns_body_for_oneliner() {
        let body = "test_one()        { lock_cargo_test foo; }\n";
        let funcs = top_level_functions(body);
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "test_one");
        assert!(funcs[0].body.contains("lock_cargo_test foo"));
    }

    /// A `{` inside a bash comment must not extend the function body —
    /// brace-balancer needs to skip comments.
    #[test]
    fn top_level_functions_ignores_braces_inside_comments() {
        let body = "\
test_a() {
    # comment with { brace
    echo ok
}
test_b() {
    echo two
}
";
        let funcs = top_level_functions(body);
        assert_eq!(funcs.len(), 2);
        assert_eq!(funcs[0].name, "test_a");
        assert_eq!(funcs[1].name, "test_b");
    }

    /// `${var#pat}` and `${var%pat}` parameter expansions look like
    /// comments / format specifiers — but `#` after `{` is bash syntax,
    /// not a comment, and must stay inside the function body.
    #[test]
    fn top_level_functions_handles_parameter_expansion() {
        let body = "\
test_a() {
    name=${pair%=*}
    val=${pair#*=}
    echo \"$name=$val\"
}
test_b() {
    echo next
}
";
        let funcs = top_level_functions(body);
        assert_eq!(funcs.len(), 2);
        assert_eq!(funcs[0].name, "test_a");
        assert!(funcs[0].body.contains("${pair#*=}"));
        assert_eq!(funcs[1].name, "test_b");
    }

    /// Backtick-quoted text inside a comment ought to be treated as
    /// quoted (its braces must not enter the brace count). This is what
    /// bit the proptest_case_count refactor when the comment had
    /// `` `proptest! {` `` inside it.
    #[test]
    fn top_level_functions_handles_backticks_in_comments() {
        let body = "\
test_a() {
    # see `proptest! {` block in properties.rs
    echo ok
}
test_b() {
    echo next
}
";
        let funcs = top_level_functions(body);
        assert_eq!(funcs.len(), 2);
    }

    #[test]
    fn bash_statements_splits_on_semicolons_and_newlines() {
        let body = "lock_cargo_test foo; lock_cargo_test bar\nlock_cargo_test baz\n";
        let stmts = bash_statements(body);
        assert_eq!(
            stmts,
            vec![
                "lock_cargo_test foo".to_string(),
                "lock_cargo_test bar".to_string(),
                "lock_cargo_test baz".to_string(),
            ],
        );
    }

    #[test]
    fn tokenize_bash_strips_quotes_and_keeps_dollar_one() {
        let toks = tokenize_bash("cargo_run test -p X --lib \"$1\" -- --exact");
        assert!(toks.contains(&"$1".to_string()));
        assert!(toks.contains(&"--lib".to_string()));
    }
}
