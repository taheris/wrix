//! Parser for the **Options Format Contract** (see `specs/ralph-review.md`).
//!
//! Clarify beads enumerate their options with two heading shapes:
//!
//! ```markdown
//! ## Options — <one-line summary, ≤50 chars>
//!
//! ### Option 1 — <short title>
//! <body>
//!
//! ### Option 2 — <short title>
//! <body>
//! ```
//!
//! Separators between `Options` / `Option N` and the trailing summary or
//! title may be em-dash `—`, en-dash `–`, single hyphen `-`, or double
//! hyphen `--`. The parser tolerates any of these.

/// Result of parsing one bead description against the contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptionsParse {
    /// One-line summary trailing the `## Options` heading. Empty when the
    /// header is absent or carries no summary.
    pub summary: String,

    /// `### Option N — <title>` subsections in source order. Empty when no
    /// numbered options are present.
    pub options: Vec<OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionEntry {
    /// 1-based numbering as authored in `### Option N`. Used by `-a <int>`
    /// fast-reply lookup.
    pub n: u32,
    pub title: String,
    pub body: String,
}

/// Parse an `## Options` block from a bead description per the contract.
///
/// Behaviour:
/// - Returns `OptionsParse::default()` when the description has no
///   `## Options` heading.
/// - Trailing summary is stripped of the leading separator (`—`, `–`, `-`,
///   `--`) before being returned.
/// - Each `### Option N` subsection extends from its heading until the next
///   `### Option` or the next `##` heading (whichever comes first).
pub fn parse_options(description: &str) -> OptionsParse {
    let mut parse = OptionsParse::default();
    let mut in_options = false;
    let mut current: Option<OptionEntry> = None;
    let mut current_body_lines: Vec<&str> = Vec::new();

    for line in description.lines() {
        if !in_options {
            if let Some(rest) = match_options_heading(line) {
                in_options = true;
                parse.summary = strip_separator(rest);
            }
            continue;
        }

        if let Some((n, rest)) = match_option_heading(line) {
            if let Some(mut entry) = current.take() {
                entry.body = join_body(&current_body_lines);
                parse.options.push(entry);
                current_body_lines.clear();
            }
            current = Some(OptionEntry {
                n,
                title: strip_separator(rest),
                body: String::new(),
            });
            continue;
        }

        if is_next_h2(line) {
            if let Some(mut entry) = current.take() {
                entry.body = join_body(&current_body_lines);
                parse.options.push(entry);
                current_body_lines.clear();
            }
            in_options = false;
            continue;
        }

        if current.is_some() {
            current_body_lines.push(line);
        }
    }

    if let Some(mut entry) = current.take() {
        entry.body = join_body(&current_body_lines);
        parse.options.push(entry);
    }

    parse
}

fn match_options_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?;
    let rest = rest.strip_prefix("Options")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn match_option_heading(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix("### ")?;
    let rest = rest.strip_prefix("Option")?;
    let rest = rest.strip_prefix(' ')?;
    let trimmed = rest.trim_start();
    let (digits, after) = take_digits(trimmed);
    if digits.is_empty() {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some((n, after.trim_start()))
}

fn take_digits(s: &str) -> (&str, &str) {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    s.split_at(end)
}

fn strip_separator(rest: &str) -> String {
    let trimmed = rest.trim_start();
    let after = if let Some(s) = trimmed.strip_prefix("—") {
        s
    } else if let Some(s) = trimmed.strip_prefix("–") {
        s
    } else if let Some(s) = trimmed.strip_prefix("--") {
        s
    } else if let Some(s) = trimmed.strip_prefix('-') {
        s
    } else {
        trimmed
    };
    after.trim().to_string()
}

fn is_next_h2(line: &str) -> bool {
    line.starts_with("## ") && !line.starts_with("### ")
}

fn join_body(lines: &[&str]) -> String {
    let mut s = lines.join("\n");
    while s.ends_with('\n') {
        s.pop();
    }
    while s.starts_with('\n') {
        s.remove(0);
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn missing_options_section_returns_default() {
        let parse = parse_options("Just a body, no options.");
        assert!(parse.summary.is_empty());
        assert!(parse.options.is_empty());
    }

    #[test]
    fn options_em_dash_summary_and_three_options() {
        let desc = "\
## Options — pick a path

### Option 1 — Preserve invariant
Revert the change. Cost: churn.

### Option 2 — Keep on top
Accept the clash. Cost: debt.

### Option 3 — Change invariant
Update the spec. Cost: realignment.
";
        let parse = parse_options(desc);
        assert_eq!(parse.summary, "pick a path");
        assert_eq!(parse.options.len(), 3);
        assert_eq!(parse.options[0].n, 1);
        assert_eq!(parse.options[0].title, "Preserve invariant");
        assert!(parse.options[0].body.contains("Revert the change"));
        assert_eq!(parse.options[2].title, "Change invariant");
    }

    #[test]
    fn separator_variants_all_strip_cleanly() {
        for sep in ["—", "–", "-", "--"] {
            let desc = format!("## Options {sep} summary text\n\n### Option 1 {sep} title\nbody\n");
            let parse = parse_options(&desc);
            assert_eq!(parse.summary, "summary text", "sep={sep}");
            assert_eq!(parse.options[0].title, "title", "sep={sep}");
        }
    }

    #[test]
    fn option_body_extends_to_next_option_heading() {
        let desc = "\
## Options

### Option 1 — first
line a
line b

### Option 2 — second
line c
";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 2);
        assert_eq!(parse.options[0].body, "line a\nline b");
        assert_eq!(parse.options[1].body, "line c");
    }

    #[test]
    fn next_h2_terminates_options_block() {
        let desc = "\
## Options — sum

### Option 1 — t1
body1

## Other section
ignored
";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].body, "body1");
    }

    #[test]
    fn options_without_summary_have_empty_summary() {
        let desc = "## Options\n\n### Option 1 — t\nbody\n";
        let parse = parse_options(desc);
        assert_eq!(parse.summary, "");
        assert_eq!(parse.options.len(), 1);
    }

    #[test]
    fn option_without_title_has_empty_title() {
        let desc = "## Options\n\n### Option 1\nbody only\n";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].title, "");
        assert_eq!(parse.options[0].body, "body only");
    }
}
