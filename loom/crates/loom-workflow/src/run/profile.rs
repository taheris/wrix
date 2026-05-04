use loom_core::bd::Label;
use loom_core::identifier::ProfileName;

/// Profile used when a bead carries no `profile:X` label and no override is
/// supplied. Matches the default in `lib/sandbox/profiles.nix`.
pub const DEFAULT_PROFILE: &str = "base";

/// Resolve the [`ProfileName`] for a bead.
///
/// Order of precedence:
/// 1. CLI `--profile` override (caller passes `Some(profile)`).
/// 2. The first `profile:X` label on the bead.
/// 3. [`DEFAULT_PROFILE`] (`base`).
///
/// Pure function — the driver hands in the labels it already pulled from
/// `bd show`.
pub fn resolve_profile(bead_labels: &[Label], override_: Option<&ProfileName>) -> ProfileName {
    if let Some(p) = override_ {
        return p.clone();
    }
    bead_labels
        .iter()
        .find_map(Label::profile_name)
        .unwrap_or_else(|| ProfileName::new(DEFAULT_PROFILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<Label> {
        items.iter().map(|s| Label::new(*s)).collect()
    }

    #[test]
    fn resolve_profile_reads_label() {
        let labels = labels(&["spec:loom-harness", "profile:rust"]);
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new("rust"));
    }

    #[test]
    fn resolve_profile_falls_back_to_base_without_label() {
        let labels = labels(&["spec:loom-harness"]);
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new(DEFAULT_PROFILE));
    }

    #[test]
    fn resolve_profile_uses_override() {
        let labels = labels(&["profile:rust"]);
        let override_ = ProfileName::new("python");
        let p = resolve_profile(&labels, Some(&override_));
        assert_eq!(p, ProfileName::new("python"));
    }

    #[test]
    fn resolve_profile_first_matching_label_wins() {
        let labels = labels(&["profile:rust", "profile:python"]);
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new("rust"));
    }
}
