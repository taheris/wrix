//! `[tier](target)` annotation parser surface.
//!
//! Walks the consumer's spec tree and extracts each `[tier](target)` token
//! attached to an acceptance-criterion line. Tiers are a closed set
//! (`check` / `test` / `system` / `judge`) per `docs/spec-conventions.md`
//! and dispatched per the rules in `specs/loom-gate.md`. The actual
//! parsing logic lands in its own bead — this scaffold exposes the type
//! surface plus a stub [`parse_annotations`] entry point.

use std::path::{Path, PathBuf};

use crate::GateError;

/// Verifier tier for one annotation. Closed set per RS-17; the wire
/// strings line up with the `[tier]` text in spec files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Static analysis — `[check](command)` invokes a verifier subprocess.
    Check,
    /// Language-native test — `[test](path)` is batched into one runner.
    Test,
    /// Container / packaging / end-to-end — `[system](command)` is its own
    /// subprocess.
    System,
    /// LLM judgement — `[judge](path)` reads a rubric file.
    Judge,
}

impl Tier {
    /// Lowercase wire string. Matches the `[tier]` text in spec files.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Tier::Check => "check",
            Tier::Test => "test",
            Tier::System => "system",
            Tier::Judge => "judge",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// One parsed `[tier](target)` annotation extracted from a spec file.
///
/// `target` is the raw string between the parentheses; resolution (whether
/// the command exists on PATH, whether the test path matches a function,
/// whether the file exists on disk) is the integrity gate's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub tier: Tier,
    pub target: String,
    pub source_spec: PathBuf,
    pub line: u32,
}

/// Walk `specs_dir` and return every annotation found. Stub — implemented
/// in the annotation-parser bead.
pub fn parse(_specs_dir: &Path) -> Result<Vec<Annotation>, GateError> {
    Err(GateError::Unimplemented {
        what: "annotation::parse",
    })
}
