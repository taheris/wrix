use loom_core::bd::{Bead, Label};
use loom_core::identifier::SpecLabel;

use super::options::parse_options;

/// One row of the outstanding-clarify list. Built from a [`Bead`] plus
/// (optionally) a spec filter that drops the SPEC column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarifyRow {
    /// 1-based sequential index, ordered by bead creation. Stable only
    /// until the visible set changes.
    pub index: u32,
    pub bead_id: String,
    /// `Some(label)` in cross-spec mode; `None` when the list is filtered to
    /// a single spec (the column is dropped).
    pub spec: Option<String>,
    /// Summary from `## Options — <summary>`; falls back to the bead title
    /// when the header is absent or empty.
    pub summary: String,
}

/// Filter beads to those still labelled `loom:clarify`. The spec filter,
/// when supplied, restricts the result to beads carrying `spec:<label>`.
/// Order is preserved from the input slice (the caller sorts by creation
/// time before calling).
pub fn filter_clarifies<'a>(beads: &'a [Bead], spec: Option<&SpecLabel>) -> Vec<&'a Bead> {
    beads
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_clarify))
        .filter(|b| match spec {
            None => true,
            Some(label) => b
                .labels
                .iter()
                .any(|l| l.spec_label().as_ref() == Some(label)),
        })
        .collect()
}

/// Build the rendered [`ClarifyRow`] table. `spec_filter` controls whether
/// the SPEC column is populated — `None` keeps it (cross-spec list) and
/// `Some(_)` drops it (the column would carry the same value for every row).
pub fn build_rows(beads: &[&Bead], spec_filter: Option<&SpecLabel>) -> Vec<ClarifyRow> {
    beads
        .iter()
        .enumerate()
        .map(|(i, bead)| {
            let parsed = parse_options(&bead.description);
            let summary = if parsed.summary.is_empty() {
                bead.title.clone()
            } else {
                parsed.summary
            };
            let spec = if spec_filter.is_some() {
                None
            } else {
                Some(
                    spec_label_of(bead)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".to_string()),
                )
            };
            ClarifyRow {
                index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                bead_id: bead.id.to_string(),
                spec,
                summary,
            }
        })
        .collect()
}

/// Extract the `spec:<label>` value from a bead's labels. `loom msg`'s
/// resume hint reads this on every successful clarify clear.
pub fn spec_label_of(bead: &Bead) -> Option<SpecLabel> {
    bead.labels.iter().find_map(Label::spec_label)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loom_core::identifier::BeadId;

    fn bead(id: &str, title: &str, desc: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: title.into(),
            description: desc.into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
        }
    }

    #[test]
    fn filter_keeps_only_clarify_labelled_beads() {
        let beads = vec![
            bead("wx-1", "no clarify", "", &["spec:loom-harness"]),
            bead(
                "wx-2",
                "with clarify",
                "",
                &["spec:loom-harness", "loom:clarify"],
            ),
            bead(
                "wx-3",
                "other spec clarify",
                "",
                &["spec:profiles", "loom:clarify"],
            ),
        ];
        let kept = filter_clarifies(&beads, None);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].id, BeadId::new("wx-2").expect("valid"));
        assert_eq!(kept[1].id, BeadId::new("wx-3").expect("valid"));
    }

    #[test]
    fn filter_with_spec_label_keeps_only_matching() {
        let beads = vec![
            bead("wx-2", "loom", "", &["spec:loom-harness", "loom:clarify"]),
            bead("wx-3", "profiles", "", &["spec:profiles", "loom:clarify"]),
        ];
        let label = SpecLabel::new("loom-harness");
        let kept = filter_clarifies(&beads, Some(&label));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, BeadId::new("wx-2").expect("valid"));
    }

    #[test]
    fn rows_drop_spec_column_under_filter() {
        let beads = vec![bead(
            "wx-2",
            "title",
            "",
            &["spec:loom-harness", "loom:clarify"],
        )];
        let label = SpecLabel::new("loom-harness");
        let kept = filter_clarifies(&beads, Some(&label));
        let rows = build_rows(&kept, Some(&label));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 1);
        assert!(rows[0].spec.is_none(), "spec dropped under filter");
    }

    #[test]
    fn rows_carry_spec_column_when_unfiltered() {
        let beads = vec![bead(
            "wx-2",
            "title",
            "",
            &["spec:loom-harness", "loom:clarify"],
        )];
        let kept = filter_clarifies(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].spec.as_deref(), Some("loom-harness"));
    }

    #[test]
    fn summary_prefers_options_header_over_title() {
        let desc = "## Options — chosen summary\n\n### Option 1 — t\nbody\n";
        let beads = vec![bead("wx-2", "fallback title", desc, &["loom:clarify"])];
        let kept = filter_clarifies(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].summary, "chosen summary");
    }

    #[test]
    fn summary_falls_back_to_title_when_header_absent() {
        let beads = vec![bead(
            "wx-2",
            "the title",
            "no options here",
            &["loom:clarify"],
        )];
        let kept = filter_clarifies(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].summary, "the title");
    }

    #[test]
    fn missing_spec_label_renders_em_dash_in_cross_spec_view() {
        let beads = vec![bead("wx-2", "t", "", &["loom:clarify"])];
        let kept = filter_clarifies(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].spec.as_deref(), Some("—"));
    }
}
