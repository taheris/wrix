//! `loom msg` — clarify resolution.
//!
//! Implements the `ralph msg`-style clarify flow defined in
//! `specs/ralph-review.md` ("Clarify resolution / `ralph msg`") on top of
//! `loom-core`'s typed `BdClient` and `loom-templates`' `msg.md` template.
//! The command:
//!
//! 1. lists outstanding beads carrying `loom:clarify`, optionally filtered
//!    to `spec:<label>` so the SPEC column collapses;
//! 2. resolves an `-n <N>` / `-i <id>` selector to a [`BeadId`];
//! 3. for `-a <choice>` either looks up `### Option <N>` per the Options
//!    Format Contract and composes a `Chose option N — title: body` note
//!    ([`reply::FastReply::Option`]), or stores the choice verbatim
//!    ([`reply::FastReply::Verbatim`]);
//! 4. for `-d` writes the canonical [`reply::DISMISS_NOTE`] and removes the
//!    `loom:clarify` label so the bead drops off the list.
//!
//! The render path uses [`build_msg_context`] to compose the typed
//! [`MsgContext`](loom_templates::msg::MsgContext) the Askama template
//! consumes.

mod context;
mod error;
mod list;
mod options;
mod reply;

pub use context::{build_msg_context, resolve_target};
pub use error::MsgError;
pub use list::{ClarifyRow, build_rows, filter_clarifies, spec_label_of};
pub use options::{OptionEntry, OptionsParse, parse_options};
pub use reply::{DISMISS_NOTE, FastReply, build_fast_reply};
