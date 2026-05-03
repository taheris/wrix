//! `loom plan` — interactive spec interview.
//!
//! `plan` is the exception to Loom's NDJSON-driven workflow. The interview is
//! a human-in-the-loop terminal session, so loom shells out to interactive
//! `wrapix run` (TTY attached) rather than `wrapix run-bead --stdio`. There
//! is no subprocess capture, no NDJSON parsing, and no event tee.
//!
//! Flow per `specs/loom-harness.md`:
//!
//! 1. parse `-n <label>` (new) or `-u <label>` (update) into [`PlanMode`];
//! 2. acquire `<label>.lock` for the duration of the call;
//! 3. render `plan_new.md` or `plan_update.md` via Askama into a typed
//!    prompt body ([`prompt::render_prompt`]);
//! 4. exec `wrapix run <workspace> claude --dangerously-skip-permissions
//!    <prompt>` with stdio inherited so claude attaches to the user's
//!    terminal ([`command::build_wrapix_argv`]);
//! 5. after the interactive session exits, parse the resulting spec markdown
//!    for `## Companions` and replace the companion rows for `label` in the
//!    state DB ([`companions::reconcile_companions`]).
//!
//! Hidden specs (Ralph's `-h` flag) are deliberately not ported — keeping a
//! spec out of git is covered by `.git/info/exclude` (see *Out of Scope* in
//! the harness spec).

mod args;
mod command;
mod companions;
mod error;
mod prompt;
mod runner;

pub use args::{PlanMode, parse_mode};
pub use command::{WRAPIX_BIN, build_wrapix_argv};
pub use companions::{CompanionReconciliation, reconcile_companions};
pub use error::PlanError;
pub use prompt::{PlanPromptInputs, render_prompt};
pub use runner::{PlanOpts, PlanReport, run};
