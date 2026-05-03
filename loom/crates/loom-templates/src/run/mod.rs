//! `loom run` template: the per-bead implementation prompt with retry context.

use askama::Template;
use loom_core::identifier::{BeadId, MoleculeId, SpecLabel};

/// Maximum length of the agent-supplied previous-failure body before truncation.
///
/// Keeps the retry prompt below the agent's effective context limit and matches
/// the cap declared in `specs/loom-harness.md`.
pub const PREVIOUS_FAILURE_MAX_LEN: usize = 4000;

/// Wrapper around the previous-failure body that enforces the truncation cap.
///
/// Held by [`RunContext::previous_failure`] so callers cannot inject an
/// unbounded blob into the retry prompt — see the `<agent-output>` markers in
/// `templates/run.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousFailure(String);

impl PreviousFailure {
    /// Construct a `PreviousFailure`, truncating bodies longer than
    /// [`PREVIOUS_FAILURE_MAX_LEN`] at the nearest char boundary.
    pub fn new(s: impl Into<String>) -> Self {
        let mut s: String = s.into();
        if s.len() > PREVIOUS_FAILURE_MAX_LEN {
            let mut cut = PREVIOUS_FAILURE_MAX_LEN;
            while !s.is_char_boundary(cut) && cut > 0 {
                cut -= 1;
            }
            s.truncate(cut);
        }
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PreviousFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Context for `loom run` executing a single bead.
#[derive(Template)]
#[template(path = "run.md", escape = "none")]
pub struct RunContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub issue_id: Option<BeadId>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub previous_failure: Option<PreviousFailure>,
    pub exit_signals: String,
}
