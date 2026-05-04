//! Parse `## Success Criteria` checklists in spec markdown.
//!
//! Mirrors `parse_spec_annotations` in `lib/ralph/cmd/util.sh`: walks the
//! file, ignores fenced code blocks, and pairs each `- [ ]`/`- [x]` checklist
//! entry with the `[verify](path#fn)` or `[judge](path#fn)` link on the next
//! non-blank line. Criteria without an annotation become entries of type
//! [`AnnotationKind::None`].

use std::fs;
use std::path::{Path, PathBuf};

use super::error::SpecError;

/// Kind of annotation attached to a [`Annotation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    /// `[verify](path#fn)` — automated verification entry point.
    Verify,
    /// `[judge](path#fn)` — model-judged entry point.
    Judge,
    /// Criterion has no machine-readable annotation.
    None,
}

/// One row from `## Success Criteria`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub criterion: String,
    pub kind: AnnotationKind,
    pub file: Option<PathBuf>,
    pub function: Option<String>,
    pub checked: bool,
}

/// Parse all annotations in `spec_path`. Returns the rows in order.
/// Returns [`SpecError::NoSuccessCriteria`] when the file has no
/// `## Success Criteria` section.
pub fn parse_spec_annotations(spec_path: &Path) -> Result<Vec<Annotation>, SpecError> {
    let body = fs::read_to_string(spec_path).map_err(|source| SpecError::Io {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let spec_dir = spec_path.parent().unwrap_or_else(|| Path::new(""));
    parse_body(&body, spec_dir).ok_or_else(|| SpecError::NoSuccessCriteria {
        path: spec_path.to_path_buf(),
    })
}

fn parse_body(body: &str, spec_dir: &Path) -> Option<Vec<Annotation>> {
    let mut out = Vec::new();
    let mut in_criteria = false;
    let mut in_fence = false;
    let mut pending: Option<(String, bool)> = None;
    let mut saw_criteria = false;

    for raw in body.lines() {
        let line = raw;
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if is_success_criteria_heading(line) {
            in_criteria = true;
            continue;
        }
        if in_criteria && is_other_h2(line) {
            if let Some((text, checked)) = pending.take() {
                out.push(Annotation {
                    criterion: text,
                    kind: AnnotationKind::None,
                    file: None,
                    function: None,
                    checked,
                });
            }
            break;
        }
        if !in_criteria {
            continue;
        }
        if let Some((checked, text)) = parse_checkbox(line) {
            if let Some((prev, prev_checked)) = pending.take() {
                out.push(Annotation {
                    criterion: prev,
                    kind: AnnotationKind::None,
                    file: None,
                    function: None,
                    checked: prev_checked,
                });
            }
            pending = Some((text, checked));
            saw_criteria = true;
            continue;
        }
        if let Some((text, checked)) = pending.as_ref()
            && let Some((kind, target)) = parse_annotation_line(line)
        {
            let (file, function) = resolve_annotation_link(&target, spec_dir);
            out.push(Annotation {
                criterion: text.clone(),
                kind,
                file: Some(file),
                function,
                checked: *checked,
            });
            pending = None;
        }
    }
    if let Some((text, checked)) = pending {
        out.push(Annotation {
            criterion: text,
            kind: AnnotationKind::None,
            file: None,
            function: None,
            checked,
        });
    }
    saw_criteria.then_some(out)
}

fn is_success_criteria_heading(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some(rest) = stripped.strip_prefix("##") else {
        return false;
    };
    let rest = rest.trim_start_matches(' ');
    rest.trim_start().starts_with("Success Criteria") || rest.starts_with("Success Criteria")
}

fn is_other_h2(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some(rest) = stripped.strip_prefix("##") else {
        return false;
    };
    if rest.starts_with('#') {
        return false;
    }
    let rest = rest.trim_start_matches(' ');
    !rest.starts_with("Success Criteria")
}

fn parse_checkbox(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("- ")?;
    let rest = rest.strip_prefix('[')?;
    let mark = rest.chars().next()?;
    let after_mark = &rest[mark.len_utf8()..];
    let after_close = after_mark.strip_prefix(']')?;
    let text = after_close.strip_prefix(' ').unwrap_or(after_close);
    let checked = match mark {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    Some((checked, text.to_string()))
}

fn parse_annotation_line(line: &str) -> Option<(AnnotationKind, String)> {
    let trimmed = line.trim_start();
    for (prefix, kind) in [
        ("[verify](", AnnotationKind::Verify),
        ("[judge](", AnnotationKind::Judge),
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix)
            && let Some(target) = capture_balanced(rest)
        {
            return Some((kind, target));
        }
    }
    None
}

fn capture_balanced(rest: &str) -> Option<String> {
    let mut depth = 1usize;
    let mut out = String::new();
    for ch in rest.chars() {
        match ch {
            '(' => {
                depth += 1;
                out.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(out);
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    None
}

fn resolve_annotation_link(target: &str, spec_dir: &Path) -> (PathBuf, Option<String>) {
    let (file_part, fn_part) = match target.split_once('#') {
        Some((f, fnname)) => (f.to_string(), Some(fnname.to_string())),
        None => match target.split_once("::") {
            Some((f, fnname)) => (f.to_string(), Some(fnname.to_string())),
            None => (target.to_string(), None),
        },
    };
    let path = if file_part.starts_with("../") {
        normalize(&spec_dir.join(&file_part))
    } else {
        PathBuf::from(file_part)
    };
    (path, fn_part.filter(|s| !s.is_empty()))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn write_spec(dir: &Path, name: &str, body: &str) -> Result<PathBuf> {
        let path = dir.join(name);
        fs::write(&path, body)?;
        Ok(path)
    }

    #[test]
    fn returns_no_criteria_error_when_section_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = write_spec(dir.path(), "x.md", "# Title\n\nbody\n")?;
        let err = parse_spec_annotations(&path)
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, SpecError::NoSuccessCriteria { .. }));
        Ok(())
    }

    #[test]
    fn pairs_checkbox_with_following_verify_link() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body =
            "# X\n\n## Success Criteria\n\n- [ ] Run thing\n  [verify](tests/x.sh#test_thing)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.kind, AnnotationKind::Verify);
        assert_eq!(row.criterion, "Run thing");
        assert_eq!(row.file.as_deref(), Some(Path::new("tests/x.sh")));
        assert_eq!(row.function.as_deref(), Some("test_thing"));
        assert!(!row.checked);
        Ok(())
    }

    #[test]
    fn checked_box_propagates() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- [x] done\n  [judge](specs/foo.md)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert!(rows[0].checked);
        assert_eq!(rows[0].kind, AnnotationKind::Judge);
        Ok(())
    }

    #[test]
    fn criterion_without_annotation_yields_none_kind() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- [ ] orphan\n- [ ] paired\n  [verify](t.sh#x)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, AnnotationKind::None);
        assert_eq!(rows[0].criterion, "orphan");
        assert_eq!(rows[1].kind, AnnotationKind::Verify);
        Ok(())
    }

    #[test]
    fn fenced_code_blocks_are_skipped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n```\n- [ ] not a real entry\n```\n\n- [ ] real\n  [verify](t.sh#x)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].criterion, "real");
        Ok(())
    }

    #[test]
    fn next_h2_terminates_section() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body =
            "## Success Criteria\n\n- [ ] a\n  [verify](t.sh#x)\n\n## Other\n\n- [ ] not parsed\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        Ok(())
    }

    #[test]
    fn relative_paths_normalize_against_spec_dir() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs)?;
        let body = "## Success Criteria\n\n- [ ] a\n  [verify](../tests/foo.sh#test_a)\n";
        let path = write_spec(&specs, "foo.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        let resolved = rows[0]
            .file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("file must be set"))?;
        assert_eq!(resolved, dir.path().join("tests/foo.sh"));
        Ok(())
    }

    #[test]
    fn legacy_double_colon_separator_supported() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- [ ] a\n  [verify](tests/foo.sh::test_a)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows[0].function.as_deref(), Some("test_a"));
        Ok(())
    }
}
