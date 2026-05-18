//! Walk registry — the name → walk function table the binary dispatches
//! against.
//!
//! Each walk implements `WalkFn`. The registry is a static slice so adding
//! a walk is a one-line edit and the dispatcher can iterate the available
//! names without runtime allocation. No walks ship in the scaffolding bead;
//! [`REGISTRY`] is intentionally empty and the walks bead populates it
//! alongside per-walk fixture coverage in `tests/fixture.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Verdict each walk returns. Matches the verifier-runner contract in
/// `specs/loom-gate.md`: a JSON line `{"pass": bool, "evidence": "<msg>"}`
/// on stdout, exit code mirroring `pass`. By convention `evidence` for a
/// `false` verdict carries `<path>:<line> <rule-id>` so reviewers can click
/// into the violation; this struct does not enforce that shape — the rule
/// lives in the spec, the type is the wire envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub pass: bool,
    pub evidence: String,
}

/// Input passed to every walk. `files` is `Some` when the gate set
/// `LOOM_FILES` (the `--files` scope); the walk filters its own scan
/// against the set. `None` means scan the walk's declared scope in full.
#[derive(Debug, Clone, Default)]
pub struct WalkInput {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the scaffolding bead registers zero walks; the walks bead reads this field from every walk implementation it adds"
        )
    )]
    pub files: Option<Vec<PathBuf>>,
}

impl WalkInput {
    /// Build from process env. An unset or empty `LOOM_FILES` resolves to
    /// `files: None` so an unfiltered run scans the walk's full scope; any
    /// non-empty value is split on `:` (the colon-separated convention from
    /// the verifier-runner contract).
    pub fn from_env() -> Self {
        let Ok(raw) = std::env::var("LOOM_FILES") else {
            return Self { files: None };
        };
        Self::parse_files(&raw)
    }

    /// Parse a raw `LOOM_FILES` value into a `WalkInput`. Empty input
    /// resolves to `files: None` so the empty-string and unset cases agree.
    pub fn parse_files(raw: &str) -> Self {
        if raw.is_empty() {
            return Self { files: None };
        }
        let files: Vec<PathBuf> = raw.split(':').map(PathBuf::from).collect();
        Self { files: Some(files) }
    }
}

/// Function pointer every walk is registered under. Walks are pure
/// functions of their input — no `&mut self`, no I/O coordination beyond
/// reading the filesystem.
pub type WalkFn = fn(&WalkInput) -> Verdict;

/// One entry in the walk registry — a stable name and the function the
/// dispatcher invokes when that name is passed on argv.
#[derive(Debug, Clone, Copy)]
pub struct Walk {
    pub name: &'static str,
    pub run: WalkFn,
}

/// Static registry of every walk this binary dispatches.
///
/// Empty in the scaffolding bead; the walks bead populates it as each
/// `[check]`-tier walk lands. Order in this slice is the order
/// [`names`] returns and the order errors enumerate available walks —
/// keep alphabetical so the error message reads stably.
pub static REGISTRY: &[Walk] = &[];

/// Look up a walk by name. Returns `None` for any name not in
/// [`REGISTRY`]; callers render the available set themselves via
/// [`names`].
pub fn lookup(name: &str) -> Option<&'static Walk> {
    REGISTRY.iter().find(|w| w.name == name)
}

/// Names of every walk in [`REGISTRY`], in registration order. Empty
/// vector when no walks are registered.
pub fn names() -> Vec<&'static str> {
    REGISTRY.iter().map(|w| w.name).collect()
}

/// Human-readable rendering of the available-walks set used in error
/// messages. `<none>` when the registry is empty so the message stays
/// readable; otherwise comma-separated.
pub fn names_pretty() -> String {
    let names = names();
    if names.is_empty() {
        "<none>".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn empty_registry_lookup_returns_none() {
        assert!(lookup("anything").is_none());
    }

    #[test]
    fn empty_registry_names_is_empty() {
        assert!(names().is_empty());
    }

    #[test]
    fn empty_registry_names_pretty_is_none_token() {
        assert_eq!(names_pretty(), "<none>");
    }

    #[test]
    fn verdict_round_trips_through_json() {
        let v = Verdict {
            pass: false,
            evidence: "src/foo.rs:12 NO_GIX".into(),
        };
        let s = serde_json::to_string(&v).unwrap();
        let back: Verdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
        assert_eq!(s, r#"{"pass":false,"evidence":"src/foo.rs:12 NO_GIX"}"#);
    }

    #[test]
    fn parse_files_none_for_empty_input() {
        assert!(WalkInput::parse_files("").files.is_none());
    }

    #[test]
    fn parse_files_splits_on_colon() {
        let input = WalkInput::parse_files("a.rs:b/c.rs:d.rs");
        let files = input.files.unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b/c.rs"),
                PathBuf::from("d.rs"),
            ]
        );
    }

    #[test]
    fn parse_files_single_path_no_colon() {
        let input = WalkInput::parse_files("src/lib.rs");
        let files = input.files.unwrap();
        assert_eq!(files, vec![PathBuf::from("src/lib.rs")]);
    }
}
