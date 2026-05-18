//! Parse `cargo_run test ...` invocations out of bash dispatchers, and
//! batch the resulting set into a single `cargo nextest` call.
//!
//! The `loom check criteria` audit's per-criterion `bash tests/loom-test.sh
//! test_X` pattern costs ~100 ms of cargo overhead even with a warm
//! incremental cache. With ~150 cargo-test dispatchers that's ~15 s of
//! pure fork/manifest overhead. nextest runs them all in one process
//! with native per-test scheduling, collapsing the per-criterion cargo
//! cost.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Cargo target a dispatcher fires at: the package name plus whether
/// the test lives in the lib or an integration-test binary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetKey {
    pub package: String,
    pub kind: TargetKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Lib,
    Test(String),
}

impl TargetKind {
    fn nextest_binary_kind(&self) -> &'static str {
        match self {
            TargetKind::Lib => "lib",
            TargetKind::Test(_) => "test",
        }
    }

    fn nextest_binary_name(&self, package: &str) -> String {
        match self {
            // nextest's `binary()` filter for a crate's lib target
            // matches the crate's library name (snake_case package).
            TargetKind::Lib => package.replace('-', "_"),
            TargetKind::Test(name) => name.clone(),
        }
    }
}

/// One dispatcher resolved to the set of (target, test-name) tuples it
/// would exercise. A criterion passes iff every entry passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub entries: Vec<TestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestEntry {
    pub target: TargetKey,
    pub test_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helper {
    pub target: TargetKey,
    /// Prefix prepended to the helper's `$1` argument when forming the
    /// test name. Empty string for the canonical `"$1"` form, non-empty
    /// for `"some::prefix::$1"` (used by `scratch_cargo_test` today).
    pub test_name_prefix: String,
}

/// Parse the `*_cargo_test()` helper functions defined in the dispatcher
/// source. Each helper has the canonical shape
/// `cargo_run test -p PKG (--test BIN | --lib) <arg-template> -- ...`
/// where `<arg-template>` is `"$1"` or `"<prefix>$1"`.
pub fn parse_helpers(source: &str) -> HashMap<String, Helper> {
    let mut out = HashMap::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let header = lines[idx].trim_start();
        if let Some(rest) = header.strip_suffix("() {")
            && let Some(name) = rest.strip_suffix("_cargo_test")
            && idx + 1 < lines.len()
            && let Some(helper) = parse_helper_body(lines[idx + 1].trim())
        {
            out.insert(format!("{name}_cargo_test"), helper);
        }
        idx += 1;
    }
    out
}

fn parse_helper_body(line: &str) -> Option<Helper> {
    let tokens = tokenize(line)?;
    let mut it = tokens.iter();
    if it.next()? != "cargo_run" || it.next()? != "test" {
        return None;
    }
    let mut package: Option<String> = None;
    let mut kind: Option<TargetKind> = None;
    let mut arg_template: Option<String> = None;
    while let Some(tok) = it.next() {
        match tok.as_str() {
            "-p" => package = it.next().cloned(),
            "--test" => kind = it.next().cloned().map(TargetKind::Test),
            "--lib" => kind = Some(TargetKind::Lib),
            "--" => break,
            other if other.contains("$1") => arg_template = Some(other.to_string()),
            _ => {}
        }
    }
    let template = arg_template?;
    let test_name_prefix = template.trim_end_matches("$1").to_string();
    Some(Helper {
        target: TargetKey {
            package: package?,
            kind: kind?,
        },
        test_name_prefix,
    })
}

/// Try to recognise a dispatcher body as a sequence of cargo-test
/// invocations (either direct or via `*_cargo_test` helpers). Returns
/// `None` for anything else — multi-line dispatchers with conditional
/// shell logic, dispatchers that shell out to `nix`, etc. — so the
/// caller falls back to bash.
pub fn parse_dispatch(body: &str, helpers: &HashMap<String, Helper>) -> Option<Invocation> {
    let statements = body_statements(body);
    if statements.is_empty() {
        return None;
    }
    let mut entries: Vec<TestEntry> = Vec::new();
    for stmt in statements {
        let tokens = tokenize(&stmt)?;
        if tokens.is_empty() {
            continue;
        }
        if let Some(helper) = helpers.get(&tokens[0]) {
            // `<helper> ARG [ARG …]` — each arg is an independent test
            // name. The bash helper template uses `$1` only, so >1 arg
            // would silently drop the rest; reject that shape rather
            // than batching a likely bug.
            if tokens.len() != 2 {
                return None;
            }
            entries.push(TestEntry {
                target: helper.target.clone(),
                test_name: format!("{}{}", helper.test_name_prefix, tokens[1]),
            });
            continue;
        }
        if tokens[0] != "cargo_run" || tokens.get(1).map(String::as_str) != Some("test") {
            return None;
        }
        let (target, names) = parse_direct(&tokens[2..])?;
        for name in names {
            entries.push(TestEntry {
                target: target.clone(),
                test_name: name,
            });
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(Invocation { entries })
}

fn parse_direct(tokens: &[String]) -> Option<(TargetKey, Vec<String>)> {
    let mut package: Option<String> = None;
    let mut kind: Option<TargetKind> = None;
    let mut after: Vec<&str> = Vec::new();
    let mut i = 0;
    let mut hit_double_dash = false;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        if hit_double_dash {
            // `--test-threads=1` opts the dispatcher into serial
            // execution because its tests share state. nextest's
            // default scheduler would parallelise them and likely
            // flake; fall back to bash to preserve semantics.
            if tok == "--test-threads=1" {
                return None;
            }
            after.push(tok);
            i += 1;
            continue;
        }
        match tok {
            "-p" => {
                package = tokens.get(i + 1).cloned();
                i += 2;
            }
            "--test" => {
                kind = tokens.get(i + 1).cloned().map(TargetKind::Test);
                i += 2;
            }
            "--lib" => {
                kind = Some(TargetKind::Lib);
                i += 1;
            }
            "--" => {
                hit_double_dash = true;
                i += 1;
            }
            "--quiet" | "--release" => i += 1,
            _ => return None, // unrecognised cargo arg → don't risk wrong batching
        }
    }
    let positionals: Vec<String> = after
        .into_iter()
        .filter(|t| !matches!(*t, "--exact" | "--nocapture" | "--quiet" | "--ignored"))
        .map(String::from)
        .collect();
    if positionals.is_empty() {
        // Zero filter ⇒ "run every test in the binary". Batching it
        // via nextest would silently widen coverage.
        return None;
    }
    Some((
        TargetKey {
            package: package?,
            kind: kind?,
        },
        positionals,
    ))
}

/// Split a dispatcher body into logical shell statements. Joins
/// backslash-continued lines, drops the function header / closing brace
/// / comments / blank lines. Whitespace is collapsed.
fn body_statements(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for raw in body.lines() {
        let line = raw.trim_end();
        if let Some(stripped) = line.strip_suffix('\\') {
            current.push_str(stripped);
            current.push(' ');
        } else {
            current.push_str(line);
            let stmt = current.trim().to_string();
            current.clear();
            if stmt.is_empty() || stmt.starts_with('#') || stmt == "}" {
                continue;
            }
            if stmt.starts_with("test_") && stmt.contains("()") && stmt.ends_with('{') {
                continue;
            }
            if let Some(inner) = unwrap_inline_function(&stmt) {
                for sub in split_semis(inner) {
                    let sub = sub.trim();
                    if !sub.is_empty() && !sub.starts_with('#') {
                        out.push(sub.to_string());
                    }
                }
                continue;
            }
            out.push(stmt);
        }
    }
    out
}

/// Peel `test_<name>()   { <body> }` (or `<name>()  { ... }`) when the
/// entire line is the wrapper. Returns the inner body without the
/// surrounding braces. Inline-brace dispatchers like
/// `test_x() { helper foo; }` go through this so the body downstream
/// looks the same as for multi-line dispatchers.
fn unwrap_inline_function(line: &str) -> Option<&str> {
    let line = line.trim();
    let paren = line.find("()")?;
    let header = &line[..paren];
    if header.is_empty()
        || !header
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let after = line[paren + 2..].trim_start();
    let inner = after.strip_prefix('{')?.trim();
    let inner = inner.strip_suffix('}')?;
    Some(inner.trim())
}

/// Split on top-level `;` (not inside quotes). Inline bodies frequently
/// pack multiple statements: `helper a; helper b; return 0`.
fn split_semis(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for c in line.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                cur.push(c);
            }
            '"' if !in_single => {
                in_double = !in_double;
                cur.push(c);
            }
            ';' if !in_single && !in_double => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Minimal POSIX-ish tokenizer: splits on whitespace, honours single
/// and double quotes, strips quote characters from contents. Returns
/// `None` for unbalanced quotes or any backtick / `$(` substitution the
/// parser can't safely interpret.
fn tokenize(line: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            '`' => return None,
            '$' if chars.peek() == Some(&'(') => return None,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_single || in_double {
        return None;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

/// Outcome of a single batched nextest run.
#[derive(Debug, Default)]
pub struct BatchOutcome {
    /// Test names (as the dispatcher named them via `--exact`) that
    /// nextest reports as failing.
    pub failed: std::collections::HashSet<String>,
    /// Combined stderr from the batched run, captured verbatim. Used
    /// as the `stderr_tail` fallback when surfacing a failing
    /// dispatcher.
    pub stderr: String,
    /// nextest's own exit code. Non-zero ⇔ at least one test failed
    /// or the harness errored.
    pub exit_code: i32,
}

/// Run `cargo nextest` once with a filterset that selects every
/// `(invocation.target, invocation.test_name)` pair, returning per-test
/// pass/fail. `workspace_dir` is the workspace root cargo resolves
/// `Cargo.toml` from.
pub fn run_batch(workspace_dir: &Path, entries: &[TestEntry]) -> std::io::Result<BatchOutcome> {
    let filterset = build_filterset(entries);
    let output = Command::new("cargo")
        .arg("nextest")
        .arg("run")
        .arg("--no-fail-fast")
        .arg("--color=never")
        .arg("--workspace")
        .arg("-E")
        .arg(&filterset)
        .current_dir(workspace_dir)
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let failed = parse_failures(&stderr);
    Ok(BatchOutcome {
        failed,
        stderr,
        exit_code: output.status.code().unwrap_or(-1),
    })
}

fn build_filterset(entries: &[TestEntry]) -> String {
    let mut clauses: Vec<String> = entries
        .iter()
        .map(|e| {
            let binary = e.target.kind.nextest_binary_name(&e.target.package);
            let kind = e.target.kind.nextest_binary_kind();
            format!(
                "(kind({kind}) & binary({binary}) & test(={name}))",
                name = e.test_name,
            )
        })
        .collect();
    clauses.sort();
    clauses.dedup();
    clauses.join(" + ")
}

/// Extract failing test names from nextest's human-readable stderr.
/// nextest's stable failure line is roughly:
/// `        FAIL [   0.005s] <binary-id> <test_path>`
/// We capture the trailing `<test_path>` so dispatchers can be looked
/// up by their `--exact` test name.
fn parse_failures(stderr: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in stderr.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("FAIL ") {
            let mut after = rest.trim_start();
            if let Some(rb) = after.find(']') {
                after = after[rb + 1..].trim_start();
            }
            if let Some(test_name) = after.split_whitespace().last() {
                out.insert(test_name.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helpers() -> HashMap<String, Helper> {
        parse_helpers(
            "state_db_cargo_test() {\n    \
             cargo_run test -p loom-driver --test state_db \"$1\" -- --exact --nocapture --quiet\n\
             }\n\
             scratch_cargo_test() {\n    \
             cargo_run test -p loom-driver --lib \"scratch::tests::$1\" -- --exact --nocapture --quiet\n\
             }\n\
             todo_cargo_test() {\n    \
             cargo_run test -p loom-workflow --lib \"$1\" -- --exact --nocapture --quiet\n\
             }\n",
        )
    }

    #[test]
    fn helper_parser_captures_test_binary_and_lib_targets() {
        let h = helpers();
        let s = h.get("state_db_cargo_test").expect("state_db");
        assert_eq!(s.target.package, "loom-driver");
        assert_eq!(s.target.kind, TargetKind::Test("state_db".into()));
        assert_eq!(s.test_name_prefix, "");
        let t = h.get("todo_cargo_test").expect("todo");
        assert_eq!(t.target.kind, TargetKind::Lib);
    }

    #[test]
    fn helper_parser_captures_concatenated_test_name_prefix() {
        let h = helpers();
        let s = h.get("scratch_cargo_test").expect("scratch");
        assert_eq!(s.test_name_prefix, "scratch::tests::");
    }

    #[test]
    fn dispatch_via_helper_prepends_prefix() {
        let h = helpers();
        let body = "test_scratch_dir_created() {\n    scratch_cargo_test scratch_dir_created\n}";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 1);
        assert_eq!(
            inv.entries[0].test_name,
            "scratch::tests::scratch_dir_created"
        );
        assert_eq!(inv.entries[0].target.package, "loom-driver");
    }

    #[test]
    fn dispatch_handles_inline_brace_single_helper() {
        let h = helpers();
        let body = "test_repin_envelope() { scratch_cargo_test repin_script_runs; }";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 1);
        assert_eq!(
            inv.entries[0].test_name,
            "scratch::tests::repin_script_runs"
        );
    }

    #[test]
    fn dispatch_handles_inline_brace_multi_statement() {
        let h = helpers();
        let body = "test_x() { state_db_cargo_test foo; state_db_cargo_test bar; }";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 2);
        assert_eq!(inv.entries[0].test_name, "foo");
        assert_eq!(inv.entries[1].test_name, "bar");
    }

    #[test]
    fn dispatch_multiple_helper_calls_yield_multiple_entries() {
        let h = helpers();
        let body = "test_state_db_lifecycle() {\n    \
            state_db_cargo_test init_creates_schema\n    \
            state_db_cargo_test rebuild_companions\n\
            }";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 2);
        assert_eq!(inv.entries[0].test_name, "init_creates_schema");
        assert_eq!(inv.entries[1].test_name, "rebuild_companions");
    }

    #[test]
    fn dispatch_direct_cargo_invocation() {
        let h = HashMap::new();
        let body = "test_x() {\n    \
             cargo_run test -p loom --test style --quiet -- nested_module_structure\n\
             }";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 1);
        assert_eq!(inv.entries[0].test_name, "nested_module_structure");
        assert_eq!(inv.entries[0].target.kind, TargetKind::Test("style".into()));
    }

    #[test]
    fn dispatch_multiple_names_in_one_cargo_invocation() {
        let h = HashMap::new();
        let body = "test_backend_selection_flag() {\n    \
             cargo_run test -p loom --test agent_flag --quiet -- \\\n        \
             loom_help_lists_agent_global_flag \\\n        \
             loom_accepts_agent_pi \\\n        \
             loom_accepts_agent_claude\n\
             }";
        let inv = parse_dispatch(body, &h).expect("parse");
        assert_eq!(inv.entries.len(), 3);
        for e in &inv.entries {
            assert_eq!(e.target.package, "loom");
            assert_eq!(e.target.kind, TargetKind::Test("agent_flag".into()));
        }
    }

    #[test]
    fn dispatch_skips_test_threads_serial() {
        let h = HashMap::new();
        // --test-threads=1 means the dispatcher relies on serial
        // execution — nextest's parallel scheduler would risk flakes.
        let body = "test_x() {\n    \
             cargo_run test -p loom --test msg_persist -- --test-threads=1 \\\n        \
             msg_option_fast_reply \\\n        \
             msg_option_out_of_range\n\
             }";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn dispatch_skips_multi_line_shell_logic() {
        let h = HashMap::new();
        let body = "test_x() {\n    \
             local out\n    \
             out=$(something)\n\
             }";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn dispatch_skips_non_cargo_calls() {
        let h = HashMap::new();
        let body = "test_x() {\n    nix build .#loom\n}";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn dispatch_skips_bodies_with_substitution() {
        let h = HashMap::new();
        let body = "test_x() {\n    cargo_run test $(echo -p loom) --test x -- name\n}";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn dispatch_skips_unrecognised_cargo_args() {
        let h = HashMap::new();
        let body = "test_x() {\n    \
             cargo_run test -p loom --test style --features foo --quiet -- name\n\
             }";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn dispatch_skips_zero_positional_filters() {
        let h = HashMap::new();
        // No test-name filter ⇒ runs every test in the binary; batching
        // it via nextest would silently widen coverage.
        let body = "test_x() {\n    cargo_run test -p loom --test cli_help --quiet\n}";
        assert!(parse_dispatch(body, &h).is_none());
    }

    #[test]
    fn filterset_emits_one_clause_per_entry() {
        let entries = vec![
            TestEntry {
                target: TargetKey {
                    package: "loom-driver".into(),
                    kind: TargetKind::Test("state_db".into()),
                },
                test_name: "init_creates_schema".into(),
            },
            TestEntry {
                target: TargetKey {
                    package: "loom-workflow".into(),
                    kind: TargetKind::Lib,
                },
                test_name: "run::tests::outer_loop".into(),
            },
        ];
        let fs = build_filterset(&entries);
        assert!(fs.contains("binary(state_db)"));
        assert!(fs.contains("test(=init_creates_schema)"));
        assert!(fs.contains("binary(loom_workflow)"));
        assert!(fs.contains("kind(lib)"));
        assert!(fs.contains(" + "));
    }

    #[test]
    fn failure_parser_extracts_trailing_test_names() {
        let stderr = "\
        FAIL [   0.005s] loom-driver::state_db init_creates_schema\n\
        PASS [   0.001s] loom-driver::state_db rebuild_companions\n\
        FAIL [   0.012s] loom-workflow run::tests::outer_loop\n";
        let failed = parse_failures(stderr);
        assert!(failed.contains("init_creates_schema"));
        assert!(failed.contains("run::tests::outer_loop"));
        assert!(!failed.contains("rebuild_companions"));
    }
}
