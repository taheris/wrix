use tracing::warn;

use crate::markdown::{Event, HeadingLevel, Tag, TagEnd, section_events};

/// Parse a spec's optional `## Implementation Notes` section per the
/// lifecycle in `specs/loom-harness.md` § "Implementation-notes lifecycle":
///
/// - Heading must be exactly `## Implementation Notes` (case-sensitive,
///   level 2).
/// - Body is the first flat bullet list inside the section.
/// - Each bullet's text content (concatenated `Text` and `Code` events) is
///   one note. Inline code, links, and emphasis collapse to plain text.
/// - Missing section yields zero notes.
/// - Empty bullets are skipped with a `warn!`.
pub fn parse_implementation_notes(content: &str) -> Vec<String> {
    let Some(events) = section_events(content, HeadingLevel::H2, |t| t == "Implementation Notes")
    else {
        return Vec::new();
    };

    let mut notes = Vec::new();
    let mut list_depth: usize = 0;
    let mut first_list_finished = false;
    let mut item_text = String::new();
    let mut in_top_item = false;

    for (event, _) in events {
        match event {
            Event::Start(Tag::List(_)) => {
                if list_depth == 0 && first_list_finished {
                    break;
                }
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    first_list_finished = true;
                }
            }
            Event::Start(Tag::Item) if list_depth == 1 => {
                in_top_item = true;
                item_text.clear();
            }
            Event::End(TagEnd::Item) if list_depth == 1 && in_top_item => {
                in_top_item = false;
                let trimmed = item_text.trim();
                if trimmed.is_empty() {
                    warn!(
                        reason = "empty bullet",
                        "skipping empty implementation note"
                    );
                } else {
                    notes.push(trimmed.to_string());
                }
            }
            Event::Text(t) if list_depth == 1 && in_top_item => {
                item_text.push_str(&t);
            }
            Event::Code(t) if list_depth == 1 && in_top_item => {
                item_text.push_str(&t);
            }
            _ => {}
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_yields_zero_notes() {
        let md = "# Spec\n\n## Architecture\n\nNo implementation notes.\n";
        assert!(parse_implementation_notes(md).is_empty());
    }

    #[test]
    fn parses_flat_bullet_list_into_notes() {
        let md = "\
# Spec

## Implementation Notes

- Touch `lib/sandbox/`
- Watch out for the `wrapix run` env contract
";
        assert_eq!(
            parse_implementation_notes(md),
            vec![
                "Touch lib/sandbox/".to_string(),
                "Watch out for the wrapix run env contract".to_string(),
            ]
        );
    }

    #[test]
    fn case_sensitive_heading_does_not_match_lowercase() {
        let md = "## implementation notes\n\n- nope\n";
        assert!(parse_implementation_notes(md).is_empty());
    }

    #[test]
    fn fenced_implementation_notes_example_is_ignored() {
        let md = "\
# Spec

```markdown
## Implementation Notes

- fake note
```

## Implementation Notes

- real note
";
        assert_eq!(
            parse_implementation_notes(md),
            vec!["real note".to_string()]
        );
    }

    #[test]
    fn stops_at_next_heading_of_any_level() {
        let md = "\
## Implementation Notes

- first note

### Sub heading

- nested-not-counted
";
        assert_eq!(
            parse_implementation_notes(md),
            vec!["first note".to_string()]
        );
    }

    #[test]
    fn empty_bullets_are_skipped() {
        let md = "\
## Implementation Notes

- valid
-
- another
";
        assert_eq!(
            parse_implementation_notes(md),
            vec!["valid".to_string(), "another".to_string()]
        );
    }
}
