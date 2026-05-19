//! Annotation integrity gate.
//!
//! Runs as part of `loom gate check`. Two directions per
//! `specs/loom-gate.md`:
//!
//! 1. **Forward** — every annotation's target is valid for its tier:
//!    `[check](cmd)` and `[system](cmd)` first token resolves on PATH or
//!    as a file in the repo; `[test](path)` resolves to a `#[test]` /
//!    `#[tokio::test]` / `proptest!` function in the workspace;
//!    `[judge](path)` resolves to a file on disk.
//! 2. **Atomic acceptance** — each criterion carries exactly one
//!    annotation. N→1 sharing (multiple criteria pointing at the same
//!    verifier) is allowed.
//!
//! Findings render in the form prescribed by the spec:
//! `<spec>:<line>: annotation [tier](<target>) — does not resolve` or
//! `<spec>:<line>: criterion carries N annotations, expected 1`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use thiserror::Error;
use walkdir::WalkDir;

use crate::annotation::{Annotation, Tier};

/// One finding surfaced by the integrity gate.
///
/// Variants line up with the directions the gate enforces: forward
/// resolution (annotation target invalid for its tier), embedded
/// cargo-test-name resolution (`[check](cargo test ... <name>)`'s test
/// name missing), and atomic acceptance (criterion carries more than one
/// annotation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityFinding {
    /// Annotation's target does not resolve for its tier.
    UnresolvedAnnotation {
        spec: PathBuf,
        line: u32,
        tier: Tier,
        target: String,
    },
    /// `[check](cargo test ... <name>)` annotation's first token resolves
    /// but `<name>` does not match any `#[test]` / `#[tokio::test]` /
    /// `proptest!` function in the workspace. Caught separately so the
    /// message can name the unresolved test rather than the whole command.
    UnresolvedCargoTestName {
        spec: PathBuf,
        line: u32,
        target: String,
        test_name: String,
    },
    /// Criterion carries more than one annotation; atomic-acceptance
    /// violated. `count` is the number of annotations attached.
    MultipleAnnotations {
        spec: PathBuf,
        line: u32,
        count: usize,
    },
}

impl std::fmt::Display for IntegrityFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedAnnotation {
                spec,
                line,
                tier,
                target,
            } => write!(
                f,
                "{}:{}: annotation [{}]({}) — does not resolve",
                spec.display(),
                line,
                tier,
                target
            ),
            Self::UnresolvedCargoTestName {
                spec,
                line,
                target,
                test_name,
            } => write!(
                f,
                "{}:{}: annotation [check]({}) — cargo test name `{}` does not resolve",
                spec.display(),
                line,
                target,
                test_name
            ),
            Self::MultipleAnnotations { spec, line, count } => write!(
                f,
                "{}:{}: criterion carries {} annotations, expected 1",
                spec.display(),
                line,
                count
            ),
        }
    }
}

/// Errors surfaced from running the integrity gate. Resolution itself
/// returns boolean verdicts via the resolver traits; this enum is reserved
/// for failures wiring those resolvers up.
#[derive(Debug, Display, Error)]
pub enum IntegrityError {
    /// failed to walk workspace under `{root}`: {source}
    WalkWorkspace {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
}

/// Resolver for `[check]` / `[system]` annotation targets.
///
/// The integrity gate decides whether the first token of the annotation
/// command resolves; the trait abstracts how that lookup happens so tests
/// can swap in deterministic fixtures.
pub trait CommandResolver {
    /// True iff `first_token` resolves on the consumer's PATH or as a
    /// file in the consumer's repo.
    fn resolves(&self, first_token: &str) -> bool;
}

/// Resolver for `[test]` annotation targets.
///
/// Production implementations walk the consumer's workspace (cargo
/// metadata + source files); tests substitute a deterministic membership
/// check.
pub trait TestPathResolver {
    /// True iff `target` names a real test function in the workspace.
    fn resolves(&self, target: &str) -> bool;
}

/// Filesystem-backed implementation of [`CommandResolver`].
///
/// Resolution order matches the spec's "first token resolves on PATH or
/// as a file in the repo (best-effort)" wording: absolute paths and paths
/// rooted under the repo are checked first, then the `PATH` environment
/// is walked. The `PATH` lookup is snapshot-on-construction so tests can
/// pin a deterministic value via [`FsCommandResolver::with_path`].
pub struct FsCommandResolver {
    repo_root: PathBuf,
    path_entries: Vec<PathBuf>,
}

impl FsCommandResolver {
    /// Construct a resolver rooted at `repo_root` using the process's
    /// current `PATH` environment variable.
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        let path_env = std::env::var_os("PATH");
        let path_entries = path_env
            .as_deref()
            .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
            .unwrap_or_default();
        Self {
            repo_root: repo_root.into(),
            path_entries,
        }
    }

    /// Construct a resolver with an explicit `PATH` value; used by tests
    /// to keep results independent of the host environment.
    pub fn with_path(repo_root: impl Into<PathBuf>, path: &str) -> Self {
        let path_entries = std::env::split_paths(path).collect();
        Self {
            repo_root: repo_root.into(),
            path_entries,
        }
    }
}

impl CommandResolver for FsCommandResolver {
    fn resolves(&self, first_token: &str) -> bool {
        if first_token.is_empty() {
            return false;
        }
        let candidate = Path::new(first_token);
        if candidate.is_absolute() {
            return candidate.exists();
        }
        if first_token.contains('/') {
            let joined = self.repo_root.join(candidate);
            if joined.exists() {
                return true;
            }
        }
        let direct = self.repo_root.join(first_token);
        if direct.exists() {
            return true;
        }
        self.path_entries
            .iter()
            .any(|dir| dir.join(first_token).exists())
    }
}

/// Workspace-scanning implementation of [`TestPathResolver`].
///
/// Walks every `.rs` file under `repo_root`, eagerly indexing every
/// function name introduced by `#[test]`, `#[tokio::test]`, or a
/// `proptest!` block. The resolver matches annotation targets by their
/// trailing path segment — `crate::module::test_name` resolves iff some
/// scanned file defines a test function named `test_name`. Per the spec
/// this is best-effort: full module-path resolution would require cargo
/// metadata plus parsing module declarations, which this scanner
/// deliberately skips in favour of zero subprocess cost.
pub struct RustWorkspaceTestResolver {
    known_leaves: HashSet<String>,
}

impl RustWorkspaceTestResolver {
    /// Walk `repo_root` and index every test function leaf name.
    pub fn scan(repo_root: &Path) -> Result<Self, IntegrityError> {
        let mut known_leaves: HashSet<String> = HashSet::new();
        for entry in WalkDir::new(repo_root).follow_links(false) {
            let entry = entry.map_err(|e| IntegrityError::WalkWorkspace {
                root: repo_root.to_path_buf(),
                source: e,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            if path.components().any(|c| c.as_os_str() == "target") {
                continue;
            }
            let Ok(body) = fs::read_to_string(path) else {
                continue;
            };
            extract_test_fn_leaves(&body, &mut known_leaves);
        }
        Ok(Self { known_leaves })
    }

    /// Construct a resolver pre-seeded with `leaves`. Useful for tests
    /// and for callers that compute the index by other means.
    pub fn from_leaves<I, S>(leaves: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            known_leaves: leaves.into_iter().map(Into::into).collect(),
        }
    }
}

impl TestPathResolver for RustWorkspaceTestResolver {
    fn resolves(&self, target: &str) -> bool {
        let Some(leaf) = test_target_leaf(target) else {
            return false;
        };
        self.known_leaves.contains(leaf)
    }
}

/// Return the trailing path segment of a `[test]` target.
///
/// Targets use language-native path syntax: `crate::module::test_name`
/// for Rust, `tests/file.py::test_name` for Python. Both shapes carry
/// the leaf at the final segment after splitting on `::` and then on
/// `/`.
fn test_target_leaf(target: &str) -> Option<&str> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_colons = trimmed.rsplit("::").next().unwrap_or(trimmed);
    let after_slash = after_colons.rsplit('/').next().unwrap_or(after_colons);
    if after_slash.is_empty() {
        None
    } else {
        Some(after_slash)
    }
}

/// Scan `source` for Rust test function names and add their leaf names
/// to `sink`. The scanner is line-oriented and intentionally light —
/// pulling in `syn` for one leaf-extraction pass is overkill given the
/// resolver only needs membership lookup.
fn extract_test_fn_leaves(source: &str, sink: &mut HashSet<String>) {
    let mut last_attr: Option<usize> = None;
    let mut proptest_depth: usize = 0;
    let mut bare_brace_depth_for_proptest: usize = 0;
    for (i, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim_start();
        if line.starts_with("#[test]")
            || line.starts_with("#[tokio::test")
            || line.starts_with("#[tokio_test")
        {
            last_attr = Some(i);
        }
        if line.starts_with("proptest!") && line.contains('{') {
            proptest_depth = proptest_depth.saturating_add(1);
            bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_add(1);
        } else if proptest_depth > 0 {
            for c in line.chars() {
                if c == '{' {
                    bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_add(1);
                } else if c == '}' {
                    bare_brace_depth_for_proptest = bare_brace_depth_for_proptest.saturating_sub(1);
                    if bare_brace_depth_for_proptest == 0 {
                        proptest_depth = proptest_depth.saturating_sub(1);
                    }
                }
            }
        }
        let attr_attached = matches!(last_attr, Some(j) if i.saturating_sub(j) <= 8);
        let in_proptest = proptest_depth > 0;
        if !attr_attached && !in_proptest {
            continue;
        }
        if let Some(name) = parse_fn_name(line) {
            sink.insert(name.to_string());
            if attr_attached {
                last_attr = None;
            }
        }
    }
}

/// Extract the identifier following `fn ` on a line, if any.
fn parse_fn_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || c == '|');
    let stripped = trimmed
        .strip_prefix("pub ")
        .or_else(|| trimmed.strip_prefix("async "))
        .unwrap_or(trimmed);
    let stripped = stripped
        .strip_prefix("async ")
        .or_else(|| stripped.strip_prefix("pub "))
        .unwrap_or(stripped);
    let rest = stripped.strip_prefix("fn ")?;
    let end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() { None } else { Some(name) }
}

/// Run both integrity directions and return every finding in document
/// order (forward findings first, then atomic-acceptance findings).
pub fn check(
    annotations: &[Annotation],
    repo_root: &Path,
    command_resolver: &dyn CommandResolver,
    test_resolver: &dyn TestPathResolver,
) -> Vec<IntegrityFinding> {
    let mut findings = check_forward(annotations, repo_root, command_resolver, test_resolver);
    findings.extend(check_atomic_acceptance(annotations));
    findings
}

/// Forward direction: every annotation's target must resolve for its
/// tier. Returns one [`IntegrityFinding::UnresolvedAnnotation`] per
/// annotation that fails to resolve, plus one
/// [`IntegrityFinding::UnresolvedCargoTestName`] for each `[check](cargo
/// test ... <name>)` whose command resolves but whose embedded test name
/// does not.
pub fn check_forward(
    annotations: &[Annotation],
    repo_root: &Path,
    command_resolver: &dyn CommandResolver,
    test_resolver: &dyn TestPathResolver,
) -> Vec<IntegrityFinding> {
    let mut out = Vec::new();
    for ann in annotations {
        let resolved = match ann.tier {
            Tier::Check | Tier::System => resolves_command(&ann.target, command_resolver),
            Tier::Test => test_resolver.resolves(&ann.target),
            Tier::Judge => resolves_judge_path(&ann.target, repo_root),
        };
        if !resolved {
            out.push(IntegrityFinding::UnresolvedAnnotation {
                spec: ann.source_spec.clone(),
                line: ann.line,
                tier: ann.tier,
                target: ann.target.clone(),
            });
            continue;
        }
        if ann.tier == Tier::Check
            && let Some(test_name) = extract_cargo_test_name(&ann.target)
            && !test_resolver.resolves(test_name)
        {
            out.push(IntegrityFinding::UnresolvedCargoTestName {
                spec: ann.source_spec.clone(),
                line: ann.line,
                target: ann.target.clone(),
                test_name: test_name.to_string(),
            });
        }
    }
    out
}

/// Atomic-acceptance direction: each criterion carries exactly one
/// annotation. Returns one [`IntegrityFinding::MultipleAnnotations`] per
/// criterion carrying two or more annotations.
pub fn check_atomic_acceptance(annotations: &[Annotation]) -> Vec<IntegrityFinding> {
    let mut by_criterion: BTreeMap<(PathBuf, u32), Vec<&Annotation>> = BTreeMap::new();
    for ann in annotations {
        by_criterion
            .entry((ann.source_spec.clone(), ann.criterion_line))
            .or_default()
            .push(ann);
    }
    let mut out = Vec::new();
    for ((spec, line), anns) in by_criterion {
        if anns.len() > 1 {
            out.push(IntegrityFinding::MultipleAnnotations {
                spec,
                line,
                count: anns.len(),
            });
        }
    }
    out
}

fn resolves_command(target: &str, command_resolver: &dyn CommandResolver) -> bool {
    let Some(first) = first_token(target) else {
        return false;
    };
    command_resolver.resolves(first)
}

fn first_token(command: &str) -> Option<&str> {
    command.split_whitespace().next()
}

/// Return the explicit test-name positional in a `cargo test [...] <name>`
/// command, or `None` when the command is not `cargo test`, has no
/// positional after the flags, or places the test name after `--`.
///
/// Flag-arity is data-driven: long flags listed in `LONG_FLAGS_WITH_ARG`
/// consume one following token; short flags in `SHORT_FLAGS_WITH_ARG` do
/// the same. `--flag=value` is one token. The first non-flag positional
/// is the test name. `--` ends scanning — args after it are runner args
/// for the test binary, not the test name itself.
fn extract_cargo_test_name(command: &str) -> Option<&str> {
    const LONG_FLAGS_WITH_ARG: &[&str] = &[
        "package",
        "bin",
        "example",
        "bench",
        "test",
        "target",
        "target-dir",
        "manifest-path",
        "features",
        "jobs",
        "profile",
        "lockfile-path",
        "config",
        "exclude",
    ];
    const SHORT_FLAGS_WITH_ARG: &[&str] = &["p", "F", "j"];

    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.first() != Some(&"cargo") || tokens.get(1) != Some(&"test") {
        return None;
    }
    let mut i = 2;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "--" {
            return None;
        }
        if let Some(long) = tok.strip_prefix("--") {
            if long.is_empty() {
                return None;
            }
            if long.contains('=') || !LONG_FLAGS_WITH_ARG.contains(&long) {
                i += 1;
            } else {
                i += 2;
            }
            continue;
        }
        if let Some(short) = tok.strip_prefix('-')
            && !short.is_empty()
        {
            if SHORT_FLAGS_WITH_ARG.contains(&short) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        return Some(tok);
    }
    None
}

fn resolves_judge_path(target: &str, repo_root: &Path) -> bool {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return false;
    }
    let p = Path::new(trimmed);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    };
    resolved.exists()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::collections::HashSet;
    use std::fs;

    use tempfile::tempdir;

    fn ann(tier: Tier, target: &str, spec: &str, line: u32, criterion_line: u32) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from(spec),
            line,
            criterion_line,
        }
    }

    struct StubCommands {
        ok: HashSet<String>,
    }

    impl StubCommands {
        fn with(items: &[&str]) -> Self {
            Self {
                ok: items.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl CommandResolver for StubCommands {
        fn resolves(&self, first_token: &str) -> bool {
            self.ok.contains(first_token)
        }
    }

    struct StubTests {
        ok: HashSet<String>,
    }

    impl StubTests {
        fn with(items: &[&str]) -> Self {
            Self {
                ok: items.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl TestPathResolver for StubTests {
        fn resolves(&self, target: &str) -> bool {
            self.ok.contains(target)
        }
    }

    #[test]
    fn unresolved_annotation_renders_per_spec_format() {
        let f = IntegrityFinding::UnresolvedAnnotation {
            spec: PathBuf::from("specs/loom-gate.md"),
            line: 42,
            tier: Tier::Check,
            target: "cargo run -p loom-gate -- self".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/loom-gate.md:42: annotation [check](cargo run -p loom-gate -- self) — does not resolve"
        );
    }

    #[test]
    fn multiple_annotations_renders_per_spec_format() {
        let f = IntegrityFinding::MultipleAnnotations {
            spec: PathBuf::from("specs/loom-tests.md"),
            line: 7,
            count: 2,
        };
        assert_eq!(
            f.to_string(),
            "specs/loom-tests.md:7: criterion carries 2 annotations, expected 1"
        );
    }

    #[test]
    fn atomic_acceptance_passes_when_each_criterion_has_one_annotation() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::ok", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 8, 7),
        ];
        assert!(check_atomic_acceptance(&annotations).is_empty());
    }

    #[test]
    fn atomic_acceptance_flags_two_annotations_on_one_criterion() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 6, 4),
        ];
        let findings = check_atomic_acceptance(&annotations);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::MultipleAnnotations {
                spec: PathBuf::from("specs/a.md"),
                line: 4,
                count: 2,
            }
        );
    }

    #[test]
    fn atomic_acceptance_counts_three_annotations_correctly() {
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "cargo run", "specs/a.md", 6, 4),
            ann(Tier::Judge, "rubrics/x.md", "specs/a.md", 7, 4),
        ];
        let findings = check_atomic_acceptance(&annotations);
        assert_eq!(findings.len(), 1);
        match &findings[0] {
            IntegrityFinding::MultipleAnnotations { count, .. } => assert_eq!(*count, 3),
            other => panic!("expected MultipleAnnotations, got {other:?}"),
        }
    }

    #[test]
    fn n_to_one_sharing_across_criteria_is_allowed() {
        let annotations = vec![
            ann(Tier::Test, "crate::shared::t", "specs/a.md", 5, 4),
            ann(Tier::Test, "crate::shared::t", "specs/a.md", 8, 7),
            ann(Tier::Test, "crate::shared::t", "specs/b.md", 3, 2),
        ];
        assert!(
            check_atomic_acceptance(&annotations).is_empty(),
            "different criteria pointing at the same verifier is allowed"
        );
    }

    #[test]
    fn forward_passes_when_every_annotation_resolves() {
        let dir = tempdir().unwrap();
        let rubric = dir.path().join("rubric.md");
        fs::write(&rubric, "rubric body").unwrap();

        let annotations = vec![
            ann(Tier::Check, "cargo run -p w", "specs/a.md", 1, 1),
            ann(Tier::System, "nix run .#x", "specs/a.md", 2, 2),
            ann(Tier::Test, "crate::a::ok", "specs/a.md", 3, 3),
            ann(Tier::Judge, "rubric.md", "specs/a.md", 4, 4),
        ];
        let cmds = StubCommands::with(&["cargo", "nix"]);
        let tests = StubTests::with(&["crate::a::ok"]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert!(findings.is_empty(), "got findings: {findings:?}");
    }

    #[test]
    fn forward_flags_check_with_unknown_first_token() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "not-on-path --do-thing",
            "specs/a.md",
            10,
            10,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                spec: PathBuf::from("specs/a.md"),
                line: 10,
                tier: Tier::Check,
                target: "not-on-path --do-thing".into(),
            }
        );
    }

    #[test]
    fn forward_flags_system_with_unknown_first_token() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::System,
            "not-on-path --boot",
            "specs/a.md",
            11,
            11,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::System,
                ..
            }
        ));
    }

    #[test]
    fn forward_flags_test_with_unknown_path() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Test,
            "crate::missing::nowhere",
            "specs/a.md",
            12,
            12,
        )];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::ok"]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Test,
                ..
            }
        ));
    }

    #[test]
    fn forward_flags_judge_when_file_absent() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(Tier::Judge, "missing.md", "specs/a.md", 13, 13)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0],
            IntegrityFinding::UnresolvedAnnotation {
                tier: Tier::Judge,
                ..
            }
        ));
    }

    #[test]
    fn forward_judge_accepts_absolute_path() {
        let dir = tempdir().unwrap();
        let rubric = dir.path().join("rubric.md");
        fs::write(&rubric, "body").unwrap();
        let target = rubric.to_string_lossy().into_owned();

        let annotations = vec![ann(Tier::Judge, &target, "specs/a.md", 14, 14)];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, Path::new("/this/is/ignored"), &cmds, &tests);
        assert!(findings.is_empty(), "absolute judge path resolved");
    }

    #[test]
    fn check_combines_forward_and_atomic_acceptance() {
        let dir = tempdir().unwrap();
        let annotations = vec![
            ann(Tier::Test, "crate::a::t", "specs/a.md", 5, 4),
            ann(Tier::Check, "missing-cmd", "specs/a.md", 6, 4),
        ];
        let cmds = StubCommands::with(&[]);
        let tests = StubTests::with(&["crate::a::t"]);
        let findings = check(&annotations, dir.path(), &cmds, &tests);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::UnresolvedAnnotation { .. })),
            "forward flag present"
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, IntegrityFinding::MultipleAnnotations { .. })),
            "atomic-acceptance flag present"
        );
    }

    #[test]
    fn fs_command_resolver_finds_binary_on_pinned_path() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("my-walk");
        fs::write(&bin, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path(dir.path(), &bin_dir.to_string_lossy());
        assert!(resolver.resolves("my-walk"));
        assert!(!resolver.resolves("not-installed"));
    }

    #[test]
    fn fs_command_resolver_finds_file_under_repo_root() {
        let dir = tempdir().unwrap();
        let scripts = dir.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        let script = scripts.join("walk.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path(dir.path(), "");
        assert!(resolver.resolves("scripts/walk.sh"));
        assert!(!resolver.resolves("scripts/missing.sh"));
    }

    #[test]
    fn fs_command_resolver_accepts_absolute_path() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("walk.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();

        let resolver = FsCommandResolver::with_path("/elsewhere", "");
        assert!(resolver.resolves(&script.to_string_lossy()));
    }

    #[test]
    fn fs_command_resolver_rejects_empty_token() {
        let resolver = FsCommandResolver::with_path("/repo", "/usr/bin");
        assert!(!resolver.resolves(""));
    }

    #[test]
    fn rust_workspace_test_resolver_finds_attribute_test() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(
            &src,
            "#[test]\nfn alpha_works() { assert!(true); }\n\n#[tokio::test]\nasync fn beta_runs() {}\n",
        )
        .unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(resolver.resolves("crate::module::alpha_works"));
        assert!(resolver.resolves("crate::module::beta_runs"));
        assert!(!resolver.resolves("crate::module::gamma_missing"));
    }

    #[test]
    fn rust_workspace_test_resolver_finds_proptest_function() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("props.rs");
        fs::write(
            &src,
            "proptest! {\n    fn parses_arbitrary_bytes(bytes in any::<Vec<u8>>()) {\n        // body\n    }\n}\n",
        )
        .unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(resolver.resolves("crate::props::parses_arbitrary_bytes"));
    }

    #[test]
    fn rust_workspace_test_resolver_skips_target_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target/debug/build/foo.rs");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "#[test]\nfn should_not_be_indexed() {}\n").unwrap();

        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(!resolver.resolves("anything::should_not_be_indexed"));
    }

    #[test]
    fn rust_workspace_test_resolver_misses_plain_fn() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.rs");
        fs::write(&src, "fn helper() { }\n").unwrap();
        let resolver = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
        assert!(!resolver.resolves("crate::module::helper"));
    }

    #[test]
    fn test_target_leaf_handles_rust_and_python_shapes() {
        assert_eq!(
            test_target_leaf("crate::module::test_name"),
            Some("test_name")
        );
        assert_eq!(
            test_target_leaf("tests/test_foo.py::test_bar"),
            Some("test_bar")
        );
        assert_eq!(test_target_leaf("solo"), Some("solo"));
        assert_eq!(test_target_leaf(""), None);
    }

    #[test]
    fn from_leaves_constructor_round_trips() {
        let resolver = RustWorkspaceTestResolver::from_leaves(["one", "two"]);
        assert!(resolver.resolves("crate::a::one"));
        assert!(resolver.resolves("crate::b::two"));
        assert!(!resolver.resolves("crate::a::three"));
    }

    #[test]
    fn extract_cargo_test_name_returns_positional_after_lib() {
        assert_eq!(
            extract_cargo_test_name(
                "cargo test -p loom-events --lib serde_round_trips_as_plain_string"
            ),
            Some("serde_round_trips_as_plain_string")
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_positional_after_named_suite() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p loom --test cli_help help_snapshot"),
            Some("help_snapshot")
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_none_when_suite_value_is_only_positional() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p loom-templates --test snapshots"),
            None
        );
    }

    #[test]
    fn extract_cargo_test_name_returns_none_for_non_cargo_test_command() {
        assert_eq!(
            extract_cargo_test_name("cargo run -p loom-walk -- single_event_channel"),
            None
        );
        assert_eq!(extract_cargo_test_name("nix run .#test-loom"), None);
        assert_eq!(extract_cargo_test_name("rg pattern"), None);
    }

    #[test]
    fn extract_cargo_test_name_stops_at_double_dash() {
        assert_eq!(
            extract_cargo_test_name("cargo test -p foo -- --nocapture"),
            None
        );
    }

    #[test]
    fn extract_cargo_test_name_handles_long_flag_with_equals() {
        assert_eq!(
            extract_cargo_test_name("cargo test --package=foo --lib my_test"),
            Some("my_test")
        );
    }

    #[test]
    fn extract_cargo_test_name_skips_long_arg_flags() {
        assert_eq!(
            extract_cargo_test_name(
                "cargo test --manifest-path /tmp/Cargo.toml --features ci my_test"
            ),
            Some("my_test")
        );
    }

    #[test]
    fn forward_flags_check_cargo_test_with_missing_test_name() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-events --lib does_not_exist",
            "specs/a.md",
            20,
            20,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["other_name"]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            IntegrityFinding::UnresolvedCargoTestName {
                spec: PathBuf::from("specs/a.md"),
                line: 20,
                target: "cargo test -p loom-events --lib does_not_exist".into(),
                test_name: "does_not_exist".into(),
            }
        );
    }

    #[test]
    fn forward_passes_when_cargo_test_name_resolves() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-gate --test integrity end_to_end_specs_dir_check_combines_both_directions",
            "specs/a.md",
            21,
            21,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&["end_to_end_specs_dir_check_combines_both_directions"]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert!(findings.is_empty(), "got findings: {findings:?}");
    }

    #[test]
    fn forward_skips_cargo_test_name_check_when_no_explicit_name() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::Check,
            "cargo test -p loom-templates --test snapshots",
            "specs/a.md",
            22,
            22,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert!(
            findings.is_empty(),
            "no positional => no name check, got: {findings:?}"
        );
    }

    #[test]
    fn forward_does_not_apply_cargo_test_check_to_system_tier() {
        let dir = tempdir().unwrap();
        let annotations = vec![ann(
            Tier::System,
            "cargo test -p loom-events --lib does_not_exist",
            "specs/a.md",
            23,
            23,
        )];
        let cmds = StubCommands::with(&["cargo"]);
        let tests = StubTests::with(&[]);
        let findings = check_forward(&annotations, dir.path(), &cmds, &tests);
        assert!(
            findings.is_empty(),
            "system tier ignores embedded cargo-test names, got: {findings:?}"
        );
    }

    #[test]
    fn unresolved_cargo_test_name_renders_per_spec_format() {
        let f = IntegrityFinding::UnresolvedCargoTestName {
            spec: PathBuf::from("specs/loom-tests.md"),
            line: 692,
            target: "cargo test -p loom --test cli_help help_snapshot".into(),
            test_name: "help_snapshot".into(),
        };
        assert_eq!(
            f.to_string(),
            "specs/loom-tests.md:692: annotation [check](cargo test -p loom --test cli_help help_snapshot) — cargo test name `help_snapshot` does not resolve"
        );
    }
}
