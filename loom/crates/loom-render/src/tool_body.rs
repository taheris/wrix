//! Per-tool summary cells + body formatters. Spec H3 (wx-h15kl).
//!
//! Each builtin tool gets a tailored one-line summary cell; verbose
//! mode (`-v`) widens the body cap to "full content". The default
//! mode caps body at **10 lines or 2 KB whichever first** with a
//! recovery hint line.
//!
//! ## Summary cells (per spec table)
//!
//! | Tool      | Cell                                                |
//! |-----------|-----------------------------------------------------|
//! | Read      | `<path>:<line-range>   <N> lines`                   |
//! | Edit      | `<path>   +<add> -<del>   diff↓`                    |
//! | Write     | `<path>   +<lines>   new file`                      |
//! | Grep      | `"<pattern>" in <path>   <N> files`                 |
//! | Glob      | `"<pattern>" in <path>   <N> files`                 |
//! | Bash      | `<cmd-truncated>   <duration> <✓|✗ exit=N>`         |
//! | WebFetch  | `<url>   <bytes> <duration> <✓|✗>`                  |
//! | WebSearch | `"<query>"   <N> results`                           |
//! | Task      | `<description>   [agent:<subagent>]   <duration>`   |
//!
//! Unknown tools fall through to a generic `<tool>` cell.

use serde_json::Value;

/// Body cap policy for the default render mode.
pub const BODY_CAP_LINES: usize = 10;
pub const BODY_CAP_BYTES: usize = 2048;

/// Build the one-line summary cell for a tool call. `tool` is the
/// builtin name; `params` is the call's argument JSON. Pure function
/// so tests can pin per-tool shape without the renderer state.
pub fn summary_cell(tool: &str, params: &Value) -> String {
    match tool {
        "Read" => read_summary(params),
        "Edit" => edit_summary(params),
        "Write" => write_summary(params),
        "Grep" => grep_or_glob_summary(tool, params),
        "Glob" => grep_or_glob_summary(tool, params),
        "Bash" => bash_summary(params),
        "WebFetch" => webfetch_summary(params),
        "WebSearch" => websearch_summary(params),
        "Task" => task_summary(params),
        other => other.to_string(),
    }
}

fn read_summary(params: &Value) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let offset = params.get("offset").and_then(Value::as_i64);
    let limit = params.get("limit").and_then(Value::as_i64);
    let range = match (offset, limit) {
        (Some(o), Some(l)) => format!(":{o}-{}", o + l),
        (Some(o), None) => format!(":{o}-"),
        _ => String::new(),
    };
    format!("Read   {path}{range}")
}

fn edit_summary(params: &Value) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let old = params
        .get("old_string")
        .and_then(Value::as_str)
        .unwrap_or("");
    let new = params
        .get("new_string")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (add, del) = diff_counts(old, new);
    format!("Edit   {path}   +{add} -{del}   diff↓")
}

fn write_summary(params: &Value) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content = params.get("content").and_then(Value::as_str).unwrap_or("");
    let lines = content.lines().count();
    format!("Write   {path}   +{lines}   new file")
}

fn grep_or_glob_summary(tool: &str, params: &Value) -> String {
    let pattern = params.get("pattern").and_then(Value::as_str).unwrap_or("");
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    format!("{tool}   \"{pattern}\" in {path}")
}

fn bash_summary(params: &Value) -> String {
    let cmd = params.get("command").and_then(Value::as_str).unwrap_or("");
    let truncated = truncate(cmd, 60);
    format!("Bash   {truncated}")
}

fn webfetch_summary(params: &Value) -> String {
    let url = params.get("url").and_then(Value::as_str).unwrap_or("");
    format!("WebFetch   {url}")
}

fn websearch_summary(params: &Value) -> String {
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    format!("WebSearch   \"{query}\"")
}

fn task_summary(params: &Value) -> String {
    let description = params
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let subagent = params
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("Task   {description}   [agent:{subagent}]")
}

/// Compute `(added, removed)` line counts between two strings via
/// imara-diff. Used by Edit summary cells.
pub fn diff_counts(old: &str, new: &str) -> (usize, usize) {
    use imara_diff::intern::InternedInput;
    use imara_diff::sink::Sink;
    use imara_diff::{Algorithm, diff};

    /// Accumulates added/removed line counts across every hunk.
    struct CountingSink {
        added: usize,
        removed: usize,
    }
    impl Sink for CountingSink {
        type Out = (usize, usize);
        fn process_change(&mut self, before: std::ops::Range<u32>, after: std::ops::Range<u32>) {
            self.added += (after.end - after.start) as usize;
            self.removed += (before.end - before.start) as usize;
        }
        fn finish(self) -> Self::Out {
            (self.added, self.removed)
        }
    }

    let input = InternedInput::new(old, new);
    diff(
        Algorithm::Histogram,
        &input,
        CountingSink {
            added: 0,
            removed: 0,
        },
    )
}

/// Truncate `s` to `max` chars, appending `…` if it exceeded.
pub fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().take_while(|c| *c != '\n').collect();
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Cap a body to 10 lines or 2 KB (whichever first). When the cap
/// trims content, appends the spec-defined recovery hint as the final
/// line so the user knows where to find the full body.
pub fn cap_body(body: &str, bead_id: &str, tool_call_id: &str) -> String {
    let mut total_bytes = 0;
    let mut kept: Vec<String> = Vec::new();
    let mut truncated = false;
    for (i, line) in body.lines().enumerate() {
        let next_bytes = total_bytes + line.len() + 1;
        if i >= BODY_CAP_LINES || next_bytes > BODY_CAP_BYTES {
            truncated = true;
            break;
        }
        kept.push(line.to_string());
        total_bytes = next_bytes;
    }
    if truncated {
        let remaining = body.lines().count() - kept.len();
        kept.push(String::new());
        kept.push(format!(
            "  [{remaining} more lines — loom logs -b {bead_id} --tool {tool_call_id}]"
        ));
    }
    kept.join("\n")
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_summary_includes_path_and_range() {
        let cell = summary_cell(
            "Read",
            &json!({"file_path": "src/lib.rs", "offset": 10, "limit": 20}),
        );
        assert!(cell.contains("src/lib.rs"), "{cell}");
        assert!(cell.contains(":10-30"), "{cell}");
    }

    #[test]
    fn edit_summary_includes_added_removed_counts() {
        let cell = summary_cell(
            "Edit",
            &json!({
                "file_path": "src/lib.rs",
                "old_string": "fn old() {}\n",
                "new_string": "fn new() {}\nfn extra() {}\n",
            }),
        );
        assert!(cell.contains("src/lib.rs"));
        assert!(cell.contains("+"));
        assert!(cell.contains("-"));
        assert!(cell.contains("diff"));
    }

    #[test]
    fn write_summary_includes_path_and_line_count() {
        let cell = summary_cell(
            "Write",
            &json!({"file_path": "src/new.rs", "content": "a\nb\nc\n"}),
        );
        assert!(cell.contains("src/new.rs"));
        assert!(cell.contains("+3"));
        assert!(cell.contains("new file"));
    }

    #[test]
    fn bash_summary_truncates_long_commands() {
        let cmd = "echo ".to_owned() + &"x".repeat(100);
        let cell = summary_cell("Bash", &json!({"command": cmd}));
        assert!(cell.contains("Bash"));
        // 60-char cap leaves room for the leading "Bash   "
        let body_part = cell.trim_start_matches("Bash").trim();
        assert!(body_part.ends_with('…'), "{cell}");
    }

    #[test]
    fn task_summary_includes_subagent_label() {
        let cell = summary_cell(
            "Task",
            &json!({
                "description": "Review changes",
                "subagent_type": "code-reviewer",
            }),
        );
        assert!(cell.contains("Task"));
        assert!(cell.contains("Review changes"));
        assert!(cell.contains("[agent:code-reviewer]"));
    }

    #[test]
    fn unknown_tool_falls_through_to_name() {
        assert_eq!(summary_cell("CustomTool", &json!({})), "CustomTool");
    }

    #[test]
    fn diff_counts_tracks_simple_replacement() {
        // imara-diff in histogram mode counts the changed-line spans.
        // A single-line replacement reports (1, 1).
        let (a, d) = diff_counts("old line\n", "new line\n");
        assert_eq!((a, d), (1, 1));
    }

    #[test]
    fn diff_counts_handles_pure_additions() {
        let (a, d) = diff_counts("a\n", "a\nb\nc\n");
        assert_eq!((a, d), (2, 0));
    }

    #[test]
    fn cap_body_keeps_short_bodies_unchanged() {
        let body = "line 1\nline 2\nline 3";
        assert_eq!(cap_body(body, "wx-1", "tc-1"), body);
    }

    #[test]
    fn cap_body_truncates_long_bodies_with_recovery_hint() {
        let body: String = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_body(&body, "wx-1", "tc-1");
        // 10 lines kept + blank + hint = 12 lines
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 12, "{out}");
        assert!(lines[0].starts_with("line 1"));
        assert!(lines[9].starts_with("line 10"));
        assert!(lines[10].is_empty(), "{out}");
        assert!(lines[11].contains("10 more lines"), "{out}");
        assert!(lines[11].contains("loom logs -b wx-1 --tool tc-1"), "{out}");
    }

    #[test]
    fn cap_body_respects_byte_cap() {
        // 5 lines of 600 bytes each = 3000 bytes > 2048 cap.
        let body: String = (1..=5)
            .map(|_| "x".repeat(600))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_body(&body, "wx-1", "tc-1");
        // Only ~3 lines fit before the byte cap trips.
        assert!(out.contains("more lines"), "{out}");
        assert!(out.len() < body.len(), "{out}");
    }
}
