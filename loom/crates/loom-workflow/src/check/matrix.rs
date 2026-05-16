//! `loom check --check=matrix` — pinning-matrix consistency audit.
//!
//! The `### Pinning Policy` table in `specs/loom-templates.md` is the
//! authoritative contract for which partials each workflow template
//! pins. Drift between matrix and code is silent: a partial dropped
//! from a template (or quietly added) does not break compilation, yet
//! it changes what the agent sees in that phase. This audit walks both
//! sides and surfaces every divergent cell.
//!
//! ## Resolution model
//!
//! Inclusion is **transitive**: the audit treats `partial/B.md`
//! included by `partial/A.md` as effectively included in any template
//! that includes `A`. The matrix tracks what content lands in the
//! rendered output, not the syntactic shape of the direct `{% include %}`
//! call.
//!
//! ## Drift kinds
//!
//! - `MissingFromCode` — spec marks a `(partial, template)` cell ✓ but
//!   the template (transitively) does not include the partial.
//! - `MissingFromMatrix` — template (transitively) includes the partial
//!   but the spec marks the cell blank or omits the partial row.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MatrixError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("templates directory not found at {0}")]
    NoTemplatesDir(PathBuf),
    #[error("spec file not found at {0}")]
    NoSpec(PathBuf),
    #[error("no pinning matrix table found under `Pinning Policy` in {0}")]
    NoMatrix(PathBuf),
    #[error("malformed pinning matrix in {path}: {reason}")]
    MalformedMatrix { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Discrepancy {
    /// Spec matrix marks ✓ but the template (transitively) doesn't
    /// include the partial.
    MissingFromCode,
    /// Template (transitively) includes the partial but the spec marks
    /// the cell blank or omits the row.
    MissingFromMatrix,
}

impl Discrepancy {
    pub fn tag(&self) -> &'static str {
        match self {
            Discrepancy::MissingFromCode => "missing-from-code",
            Discrepancy::MissingFromMatrix => "missing-from-matrix",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixFinding {
    pub partial: String,
    pub template: String,
    pub kind: Discrepancy,
}

/// Parsed pinning matrix: per-cell truth + the column / row order
/// recovered from the spec table.
#[derive(Debug, Clone)]
pub struct PinningMatrix {
    /// `(partial, template) → marked`. `marked = true` ↔ cell holds ✓.
    pub cells: BTreeMap<(String, String), bool>,
    /// Templates declared as columns, in matrix order.
    pub templates: Vec<String>,
    /// Partials declared as rows, in matrix order.
    pub partials: Vec<String>,
}

const PINNING_HEADING_PREFIX: &str = "### Pinning Policy";

/// Parse the `### Pinning Policy` table out of `specs/loom-templates.md`.
pub fn parse_pinning_matrix(spec_path: &Path) -> Result<PinningMatrix, MatrixError> {
    if !spec_path.is_file() {
        return Err(MatrixError::NoSpec(spec_path.to_path_buf()));
    }
    let body = fs::read_to_string(spec_path).map_err(|source| MatrixError::ReadFile {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let lines: Vec<&str> = body.lines().collect();

    let heading_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(PINNING_HEADING_PREFIX))
        .ok_or_else(|| MatrixError::NoMatrix(spec_path.to_path_buf()))?;

    let header_idx = lines
        .iter()
        .enumerate()
        .skip(heading_idx + 1)
        .find(|(_, l)| l.trim_start().starts_with("| Partial "))
        .map(|(i, _)| i)
        .ok_or_else(|| MatrixError::NoMatrix(spec_path.to_path_buf()))?;

    let separator_idx = header_idx + 1;
    if separator_idx >= lines.len() || !is_table_separator(lines[separator_idx]) {
        return Err(MatrixError::MalformedMatrix {
            path: spec_path.to_path_buf(),
            reason: "expected `|---|...` separator under header row".into(),
        });
    }

    let header_cells = split_table_row(lines[header_idx]);
    if header_cells.len() < 2 {
        return Err(MatrixError::MalformedMatrix {
            path: spec_path.to_path_buf(),
            reason: format!(
                "header row has {} cells, want at least 2",
                header_cells.len()
            ),
        });
    }
    let templates: Vec<String> = header_cells[1..]
        .iter()
        .map(|c| strip_inline_code(c).to_string())
        .collect();

    let mut partials: Vec<String> = Vec::new();
    let mut cells: BTreeMap<(String, String), bool> = BTreeMap::new();

    for line in &lines[separator_idx + 1..] {
        if !line.trim_start().starts_with('|') {
            break;
        }
        let row = split_table_row(line);
        if row.is_empty() {
            continue;
        }
        let partial = strip_inline_code(&row[0]).to_string();
        if partial.is_empty() {
            return Err(MatrixError::MalformedMatrix {
                path: spec_path.to_path_buf(),
                reason: format!("empty partial cell in row: {line}"),
            });
        }
        if row.len() != templates.len() + 1 {
            return Err(MatrixError::MalformedMatrix {
                path: spec_path.to_path_buf(),
                reason: format!(
                    "row `{partial}` has {got} cells, header declares {expected} \
                     template columns",
                    got = row.len() - 1,
                    expected = templates.len(),
                ),
            });
        }
        for (tpl, cell) in templates.iter().zip(row[1..].iter()) {
            let marked = cell.trim() == "✓";
            cells.insert((partial.clone(), tpl.clone()), marked);
        }
        partials.push(partial);
    }

    if partials.is_empty() {
        return Err(MatrixError::MalformedMatrix {
            path: spec_path.to_path_buf(),
            reason: "matrix has zero partial rows".into(),
        });
    }

    Ok(PinningMatrix {
        cells,
        templates,
        partials,
    })
}

fn is_table_separator(line: &str) -> bool {
    let l = line.trim();
    if !l.starts_with('|') || !l.ends_with('|') {
        return false;
    }
    l.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .all(|s| s.chars().all(|c| c == '-' || c == ':'))
}

fn split_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let stripped = trimmed
        .strip_prefix('|')
        .and_then(|s| s.strip_suffix('|'))
        .unwrap_or(trimmed);
    stripped.split('|').map(|s| s.trim().to_string()).collect()
}

fn strip_inline_code(s: &str) -> &str {
    s.trim()
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(s.trim())
}

/// Resolve every template's full (transitive) partial closure. Keys
/// are template stems (`run`, `plan_new`, …); values are partial
/// filenames (`context_pinning.md`, …) to match the matrix-row form.
///
/// Walks each top-level `.md` file in `templates_dir` (skipping the
/// `partial/` subdirectory), parses out `{% include "partial/<name>.md" %}`
/// directives, and recursively expands each included partial's own
/// includes.
pub fn template_partials(
    templates_dir: &Path,
) -> Result<BTreeMap<String, BTreeSet<String>>, MatrixError> {
    if !templates_dir.is_dir() {
        return Err(MatrixError::NoTemplatesDir(templates_dir.to_path_buf()));
    }
    let partial_dir = templates_dir.join("partial");
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in read_dir(templates_dir)? {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let direct = read_includes(&path)?;
        let mut closure: BTreeSet<String> = BTreeSet::new();
        let mut frontier: Vec<String> = direct.into_iter().collect();
        while let Some(name) = frontier.pop() {
            if !closure.insert(name.clone()) {
                continue;
            }
            let partial_path = partial_dir.join(&name);
            if partial_path.is_file() {
                for next in read_includes(&partial_path)? {
                    if !closure.contains(&next) {
                        frontier.push(next);
                    }
                }
            }
        }
        out.insert(stem, closure);
    }
    Ok(out)
}

fn read_dir(p: &Path) -> Result<Vec<fs::DirEntry>, MatrixError> {
    let iter = fs::read_dir(p).map_err(|source| MatrixError::ReadFile {
        path: p.to_path_buf(),
        source,
    })?;
    let mut entries: Vec<fs::DirEntry> = iter.flatten().collect();
    entries.sort_by_key(|e| e.path());
    Ok(entries)
}

fn read_includes(path: &Path) -> Result<BTreeSet<String>, MatrixError> {
    let body = fs::read_to_string(path).map_err(|source| MatrixError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parse_includes(&body))
}

fn parse_includes(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "{% include \"partial/";
    let mut rest = body;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        match after.find(".md\"") {
            Some(end) => {
                let stem = &after[..end];
                if !stem.is_empty()
                    && stem
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    out.insert(format!("{stem}.md"));
                }
                rest = &after[end + 4..];
            }
            None => break,
        }
    }
    out
}

/// Full audit: matrix-cell diff between spec and code.
pub fn audit(spec_path: &Path, templates_dir: &Path) -> Result<Vec<MatrixFinding>, MatrixError> {
    let matrix = parse_pinning_matrix(spec_path)?;
    let code = template_partials(templates_dir)?;
    let mut findings: Vec<MatrixFinding> = Vec::new();

    // All partials we know about — union of matrix rows and any partial
    // we see in code (so we surface partials the matrix forgot).
    let mut all_partials: BTreeSet<String> = matrix.partials.iter().cloned().collect();
    for set in code.values() {
        for p in set {
            all_partials.insert(p.clone());
        }
    }

    for partial in &all_partials {
        for template in &matrix.templates {
            let spec_marked = matrix
                .cells
                .get(&(partial.clone(), template.clone()))
                .copied()
                .unwrap_or(false);
            let code_includes = code
                .get(template)
                .map(|s| s.contains(partial))
                .unwrap_or(false);
            match (spec_marked, code_includes) {
                (true, false) => findings.push(MatrixFinding {
                    partial: partial.clone(),
                    template: template.clone(),
                    kind: Discrepancy::MissingFromCode,
                }),
                (false, true) => findings.push(MatrixFinding {
                    partial: partial.clone(),
                    template: template.clone(),
                    kind: Discrepancy::MissingFromMatrix,
                }),
                _ => {}
            }
        }
    }

    findings.sort_by(|a, b| {
        a.kind
            .tag()
            .cmp(b.kind.tag())
            .then(a.partial.cmp(&b.partial))
            .then(a.template.cmp(&b.template))
    });
    Ok(findings)
}

/// Print findings to stderr; return the process exit code. `0` means no
/// drift; `1` means at least one cell diverges.
pub fn report(findings: &[MatrixFinding]) -> i32 {
    for f in findings {
        let msg = match f.kind {
            Discrepancy::MissingFromCode => format!(
                "spec marks `{partial}` ✓ for `{template}` but the template does \
                 not (transitively) include it",
                partial = f.partial,
                template = f.template,
            ),
            Discrepancy::MissingFromMatrix => format!(
                "template `{template}` (transitively) includes `{partial}` but the \
                 spec matrix marks the cell blank",
                partial = f.partial,
                template = f.template,
            ),
        };
        eprintln!("{tag} {msg}", tag = f.kind.tag().to_uppercase());
    }
    eprintln!(
        "loom check --check=matrix: {n} discrepanc{plural}",
        n = findings.len(),
        plural = if findings.len() == 1 { "y" } else { "ies" },
    );
    if findings.is_empty() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, body).expect("write");
        path
    }

    fn matrix_spec(body: &str) -> String {
        format!("# Loom Templates\n\n## Architecture\n\n### Pinning Policy\n\n{body}\n\n## Other\n")
    }

    #[test]
    fn parse_matrix_reads_header_and_rows_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `plan_new` | `run` |\n\
                 |---|:-:|:-:|\n\
                 | `context_pinning.md` | ✓ | ✓ |\n\
                 | `style_rules.md` |  | ✓ |\n",
            ),
        );
        let m = parse_pinning_matrix(&spec).expect("parse");
        assert_eq!(m.templates, vec!["plan_new", "run"]);
        assert_eq!(m.partials, vec!["context_pinning.md", "style_rules.md"]);
        assert!(m.cells[&("context_pinning.md".into(), "plan_new".into())]);
        assert!(m.cells[&("context_pinning.md".into(), "run".into())]);
        assert!(!m.cells[&("style_rules.md".into(), "plan_new".into())]);
        assert!(m.cells[&("style_rules.md".into(), "run".into())]);
    }

    #[test]
    fn parse_matrix_rejects_missing_heading() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(dir.path(), "loom-templates.md", "# No matrix here\n");
        let err = parse_pinning_matrix(&spec).expect_err("must fail");
        assert!(matches!(err, MatrixError::NoMatrix(_)), "got: {err:?}");
    }

    #[test]
    fn parse_matrix_rejects_uneven_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `plan_new` | `run` |\n\
                 |---|:-:|:-:|\n\
                 | `context_pinning.md` | ✓ |\n",
            ),
        );
        let err = parse_pinning_matrix(&spec).expect_err("must fail");
        assert!(
            matches!(err, MatrixError::MalformedMatrix { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn parse_includes_extracts_partial_names() {
        let body = "{% include \"partial/foo.md\" %}\n\
                    text\n\
                    {% include \"partial/bar.md\" %}\n";
        let got = parse_includes(body);
        assert!(got.contains("foo.md"));
        assert!(got.contains("bar.md"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn parse_includes_ignores_non_partial_paths() {
        // Whatever doesn't match partial/<name>.md is ignored.
        let body = "{% include \"not_partial/foo.md\" %}\n";
        let got = parse_includes(body);
        assert!(got.is_empty());
    }

    #[test]
    fn template_partials_resolves_transitively() {
        let dir = tempfile::tempdir().expect("tempdir");
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(
            &templates,
            "plan_new.md",
            "{% include \"partial/outer.md\" %}\n",
        );
        write(
            &templates,
            "partial/outer.md",
            "outer body\n{% include \"partial/inner.md\" %}\n",
        );
        write(&templates, "partial/inner.md", "inner body\n");
        let resolved = template_partials(&templates).expect("walk");
        let plan_new = &resolved["plan_new"];
        assert!(plan_new.contains("outer.md"));
        assert!(
            plan_new.contains("inner.md"),
            "transitive include must surface",
        );
    }

    #[test]
    fn template_partials_skips_partial_subdir_as_top_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(&templates, "run.md", "no includes here\n");
        write(&templates, "partial/foo.md", "should not be keyed\n");
        let resolved = template_partials(&templates).expect("walk");
        assert!(resolved.contains_key("run"));
        assert!(
            !resolved.contains_key("foo"),
            "partials must not be keyed as top-level templates",
        );
    }

    #[test]
    fn audit_flags_missing_from_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `run` |\n\
                 |---|:-:|\n\
                 | `style_rules.md` | ✓ |\n",
            ),
        );
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(&templates, "run.md", "no style_rules include here\n");
        let f = audit(&spec, &templates).expect("audit");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Discrepancy::MissingFromCode);
        assert_eq!(f[0].partial, "style_rules.md");
        assert_eq!(f[0].template, "run");
    }

    #[test]
    fn audit_flags_missing_from_matrix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `run` |\n\
                 |---|:-:|\n\
                 | `style_rules.md` |  |\n",
            ),
        );
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(
            &templates,
            "run.md",
            "{% include \"partial/style_rules.md\" %}\n",
        );
        let f = audit(&spec, &templates).expect("audit");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Discrepancy::MissingFromMatrix);
    }

    #[test]
    fn audit_flags_partial_not_in_matrix_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `run` |\n\
                 |---|:-:|\n\
                 | `context_pinning.md` | ✓ |\n",
            ),
        );
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(
            &templates,
            "run.md",
            "{% include \"partial/context_pinning.md\" %}\n\
             {% include \"partial/surprise.md\" %}\n",
        );
        write(&templates, "partial/context_pinning.md", "ctx\n");
        write(&templates, "partial/surprise.md", "boo\n");
        let f = audit(&spec, &templates).expect("audit");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Discrepancy::MissingFromMatrix);
        assert_eq!(f[0].partial, "surprise.md");
    }

    #[test]
    fn audit_clean_matrix_returns_no_findings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = write(
            dir.path(),
            "loom-templates.md",
            &matrix_spec(
                "| Partial | `run` |\n\
                 |---|:-:|\n\
                 | `context_pinning.md` | ✓ |\n",
            ),
        );
        let templates = dir.path().join("templates");
        fs::create_dir_all(templates.join("partial")).expect("mkdir");
        write(
            &templates,
            "run.md",
            "{% include \"partial/context_pinning.md\" %}\n",
        );
        write(&templates, "partial/context_pinning.md", "ctx\n");
        let f = audit(&spec, &templates).expect("audit");
        assert!(f.is_empty(), "expected clean audit, got: {f:?}");
    }

    #[test]
    fn report_exit_code_zero_for_clean_audit() {
        assert_eq!(report(&[]), 0);
    }

    #[test]
    fn report_exit_code_one_for_any_drift() {
        let f = vec![MatrixFinding {
            partial: "x.md".into(),
            template: "run".into(),
            kind: Discrepancy::MissingFromCode,
        }];
        assert_eq!(report(&f), 1);
    }
}
