use tracing::warn;

const HEADING: &str = "## Companions";

/// Parse a spec's `## Companions` section per the rules in
/// `specs/loom-harness.md`:
///
/// - Heading must be exactly `## Companions` (case-sensitive, level 2).
/// - Body is a flat bullet list of `- ` lines.
/// - Each path is the single token between a pair of backticks.
/// - Paths normalized to repo-relative POSIX (leading `/` stripped).
/// - Missing section yields zero rows.
/// - Malformed lines are skipped with a `warn!` rather than aborting.
pub fn parse_companions(content: &str) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start) = lines.iter().position(|l| *l == HEADING) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for line in &lines[start + 1..] {
        if is_heading(line) {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with("- ") {
            continue;
        }
        match extract_backtick_path(&trimmed[2..]) {
            Ok(p) => paths.push(p),
            Err(reason) => warn!(line = %line, reason, "skipping malformed companion entry"),
        }
    }
    paths
}

fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let hash_count = trimmed.bytes().take_while(|b| *b == b'#').count();
    hash_count > 0 && trimmed[hash_count..].starts_with(' ')
}

fn extract_backtick_path(body: &str) -> Result<String, &'static str> {
    let backtick_count = body.chars().filter(|c| *c == '`').count();
    if backtick_count == 0 {
        return Err("no backticks");
    }
    if backtick_count != 2 {
        return Err("expected exactly one backticked path");
    }
    let start = body.find('`').ok_or("no opening backtick")?;
    let end_rel = body[start + 1..].find('`').ok_or("no closing backtick")?;
    let raw = &body[start + 1..start + 1 + end_rel];
    if raw.is_empty() {
        return Err("empty path");
    }
    Ok(normalize(raw))
}

fn normalize(raw: &str) -> String {
    raw.trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_yields_zero_paths() {
        let md = "# Spec\n\nSome body.\n\n## Architecture\n\n- nothing here\n";
        assert!(parse_companions(md).is_empty());
    }

    #[test]
    fn parses_flat_bullet_list_with_backticked_paths() {
        let md = "\
# Spec

## Companions

- `lib/sandbox/`
- `lib/ralph/template/`

## Next Section

- `not/companion/`
";
        assert_eq!(
            parse_companions(md),
            vec!["lib/sandbox/", "lib/ralph/template/"]
        );
    }

    #[test]
    fn strips_leading_slash_keeps_trailing_slash() {
        let md = "## Companions\n\n- `/abs/path/`\n- `relative`\n";
        assert_eq!(parse_companions(md), vec!["abs/path/", "relative"]);
    }

    #[test]
    fn skips_malformed_lines_without_aborting() {
        let md = "\
## Companions

- `ok/path/`
- no backticks here
- `multiple` `paths`
- `another/ok`
";
        assert_eq!(parse_companions(md), vec!["ok/path/", "another/ok"]);
    }

    #[test]
    fn case_sensitive_heading_does_not_match_companions_lowercase() {
        let md = "## companions\n\n- `nope/`\n";
        assert!(parse_companions(md).is_empty());
    }

    #[test]
    fn ignores_text_outside_backticks_on_bullet_lines() {
        let md = "## Companions\n\n- the path is `lib/foo/` (used by foo)\n";
        assert_eq!(parse_companions(md), vec!["lib/foo/"]);
    }

    #[test]
    fn stops_at_next_heading_of_any_level() {
        let md = "\
## Companions

- `a/`

### sub heading

- `should/not/parse/`
";
        assert_eq!(parse_companions(md), vec!["a/"]);
    }
}
