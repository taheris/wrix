//! `loom msg` template: drafter session for resolving outstanding clarify beads.

use askama::Template;
use loom_core::identifier::{BeadId, SpecLabel};

/// Context for `loom msg` rendering the cross-spec clarify queue.
#[derive(Template)]
#[template(path = "msg.md", escape = "none")]
pub struct MsgContext {
    pub pinned_context: String,
    pub clarify_beads: Vec<ClarifyBead>,
    pub exit_signals: String,
}

/// A single outstanding `ralph:clarify` bead surfaced to the drafter session.
///
/// Mirrors the shape `lib/ralph/cmd/msg.sh::build_clarify_beads_block` produces
/// — id, owning spec, title, an optional `## Options — <summary>` framing,
/// and the parsed option list.
#[derive(Debug, Clone)]
pub struct ClarifyBead {
    pub id: BeadId,
    pub spec_label: SpecLabel,
    pub title: String,
    pub options_summary: Option<String>,
    pub options: Vec<ClarifyOption>,
}

/// A single option under a clarify bead's `## Options` block.
#[derive(Debug, Clone)]
pub struct ClarifyOption {
    pub n: u32,
    pub title: Option<String>,
    pub body: Option<String>,
}
