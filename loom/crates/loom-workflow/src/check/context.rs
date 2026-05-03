use loom_core::bd::Bead;
use loom_core::identifier::{MoleculeId, SpecLabel};
use loom_templates::check::CheckContext;

/// Inputs for [`build_check_context`]. Constructed once per `loom check`
/// invocation; the reviewer only runs once per molecule per gate pass.
pub struct CheckContextInputs {
    pub label: SpecLabel,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub base_commit: Option<String>,
    pub beads_summary: Option<String>,
    pub exit_signals: String,
}

/// Render the typed [`CheckContext`] used by the `check.md` Askama template.
pub fn build_check_context(inputs: CheckContextInputs) -> CheckContext {
    CheckContext {
        pinned_context: inputs.pinned_context,
        label: inputs.label,
        spec_path: inputs.spec_path,
        companion_paths: inputs.companion_paths,
        beads_summary: inputs.beads_summary,
        base_commit: inputs.base_commit,
        molecule_id: inputs.molecule_id,
        exit_signals: inputs.exit_signals,
    }
}

/// Render `BEADS_SUMMARY` for the reviewer prompt. One line per bead in the
/// molecule: `- <id>: <title> [<status>]`. Returns `None` when `beads` is
/// empty so the template can render the em-dash placeholder; the reviewer
/// is expected to read full descriptions on demand via `bd show`.
pub fn beads_summary(beads: &[Bead]) -> Option<String> {
    if beads.is_empty() {
        return None;
    }
    let mut s = String::new();
    for bead in beads {
        s.push_str(&format!(
            "- {}: {} [{}]\n",
            bead.id, bead.title, bead.status
        ));
    }
    while s.ends_with('\n') {
        s.pop();
    }
    Some(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use askama::Template;
    use loom_core::identifier::BeadId;

    fn b(id: &str, title: &str, status: &str) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: title.into(),
            description: String::new(),
            status: status.into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
        }
    }

    fn inputs() -> CheckContextInputs {
        CheckContextInputs {
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            base_commit: Some("abc123".into()),
            beads_summary: Some("- wx-1: First [open]".into()),
            exit_signals: String::new(),
        }
    }

    #[test]
    fn beads_summary_returns_none_for_empty_input() {
        assert!(beads_summary(&[]).is_none());
    }

    #[test]
    fn beads_summary_lines_carry_id_title_status() {
        let beads = vec![b("wx-1", "Plan", "open"), b("wx-2", "Run", "in_progress")];
        let s = beads_summary(&beads).expect("beads present");
        assert!(s.contains("wx-1: Plan [open]"));
        assert!(s.contains("wx-2: Run [in_progress]"));
        assert!(!s.ends_with('\n'), "trailing newline trimmed");
    }

    #[test]
    fn rendered_template_includes_label_and_base_commit() {
        let ctx = build_check_context(inputs());
        let body = ctx.render().expect("render");
        assert!(body.contains("loom-harness"), "{body}");
        assert!(body.contains("abc123"), "{body}");
        assert!(body.contains("wx-3hhwq"), "{body}");
    }

    #[test]
    fn rendered_template_renders_em_dash_for_missing_base_commit() {
        let mut i = inputs();
        i.base_commit = None;
        let ctx = build_check_context(i);
        let body = ctx.render().expect("render");
        // The check.md template uses an em-dash placeholder for the None
        // arm of base_commit / molecule_id.
        assert!(body.contains("Base commit**: —"), "{body}");
    }
}
