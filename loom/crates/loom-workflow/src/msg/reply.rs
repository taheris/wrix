use loom_core::identifier::BeadId;

use super::error::MsgError;
use super::options::{OptionsParse, parse_options};

/// What `loom msg -a <choice>` should write to the bead. `Option` is the
/// composed `Chose option N — title: body` note from a successful integer
/// lookup; `Verbatim` carries any non-integer choice unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastReply {
    Option { n: u32, note: String },
    Verbatim { note: String },
}

/// Compose the bead note for a `-a <choice>` fast-reply.
///
/// - Pure-integer `choice` → look up `### Option <choice>` in the parsed
///   options. Match → `FastReply::Option`; miss → [`MsgError::OptionMissing`]
///   carrying the available indices for the user-facing error message.
/// - Anything else → `FastReply::Verbatim` (stored unchanged in notes).
pub fn build_fast_reply(
    bead: &BeadId,
    choice: &str,
    description: &str,
) -> Result<FastReply, MsgError> {
    if let Ok(n) = choice.parse::<u32>() {
        let parsed = parse_options(description);
        return resolve_option(bead, n, &parsed);
    }
    Ok(FastReply::Verbatim {
        note: choice.to_string(),
    })
}

fn resolve_option(bead: &BeadId, n: u32, parsed: &OptionsParse) -> Result<FastReply, MsgError> {
    if let Some(opt) = parsed.options.iter().find(|o| o.n == n) {
        let note = if opt.title.is_empty() {
            format!("Chose option {n}: {}", opt.body)
        } else if opt.body.is_empty() {
            format!("Chose option {n} — {}", opt.title)
        } else {
            format!("Chose option {n} — {}: {}", opt.title, opt.body)
        };
        Ok(FastReply::Option { n, note })
    } else {
        let available = parsed
            .options
            .iter()
            .map(|o| o.n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(MsgError::OptionMissing {
            bead: bead.to_string(),
            option: n,
            available,
        })
    }
}

/// Note written by `-d` (dismiss). Mirrors ralph's wording so beads carry a
/// recognisable marker after the label is removed.
pub const DISMISS_NOTE: &str =
    "Dismissed via loom msg -d. Agent should work around the open question.";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn desc() -> &'static str {
        "## Options — pick a path

### Option 1 — Preserve invariant
Revert. Cost: churn.

### Option 2 — Keep on top
Accept. Cost: debt.
"
    }

    #[test]
    fn integer_choice_resolves_to_option_note() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x");
        let reply = build_fast_reply(&bead, "1", desc())?;
        match reply {
            FastReply::Option { n, note } => {
                assert_eq!(n, 1);
                assert!(note.contains("Chose option 1"));
                assert!(note.contains("Preserve invariant"));
                assert!(note.contains("Revert"));
            }
            other => panic!("expected Option, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn missing_option_index_errors_with_available_list() {
        let bead = BeadId::new("wx-x");
        let err = build_fast_reply(&bead, "9", desc()).expect_err("expected error");
        match err {
            MsgError::OptionMissing {
                bead,
                option,
                available,
            } => {
                assert_eq!(bead, "wx-x");
                assert_eq!(option, 9);
                assert_eq!(available, "1, 2");
            }
            other => panic!("expected OptionMissing, got {other:?}"),
        }
    }

    #[test]
    fn verbatim_string_passes_through_unchanged() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x");
        let reply = build_fast_reply(&bead, "free-form answer", desc())?;
        match reply {
            FastReply::Verbatim { note } => assert_eq!(note, "free-form answer"),
            other => panic!("expected Verbatim, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn integer_with_no_options_section_errors() {
        let bead = BeadId::new("wx-x");
        let err =
            build_fast_reply(&bead, "1", "no options at all").expect_err("expected missing option");
        assert!(matches!(err, MsgError::OptionMissing { .. }));
    }

    #[test]
    fn empty_title_or_body_renders_partial_note() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x");
        let no_title = "## Options\n\n### Option 1\nonly body\n";
        let reply = build_fast_reply(&bead, "1", no_title)?;
        match reply {
            FastReply::Option { note, .. } => {
                assert!(note.contains("Chose option 1"));
                assert!(note.contains("only body"));
            }
            other => panic!("expected Option, got {other:?}"),
        }
        Ok(())
    }
}
