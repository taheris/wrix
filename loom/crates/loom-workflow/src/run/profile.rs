use loom_core::identifier::ProfileName;

/// Bead label prefix that announces the wrapix profile a bead should run
/// under (`profile:rust`, `profile:python`, `profile:base`).
pub const PROFILE_LABEL_PREFIX: &str = "profile:";

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
pub fn resolve_profile(bead_labels: &[String], override_: Option<&ProfileName>) -> ProfileName {
    if let Some(p) = override_ {
        return p.clone();
    }
    for label in bead_labels {
        if let Some(name) = label.strip_prefix(PROFILE_LABEL_PREFIX) {
            return ProfileName::new(name);
        }
    }
    ProfileName::new(DEFAULT_PROFILE)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn resolve_profile_reads_label() {
        let labels = vec!["spec:loom-harness".into(), "profile:rust".into()];
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new("rust"));
    }

    #[test]
    fn resolve_profile_falls_back_to_base_without_label() {
        let labels: Vec<String> = vec!["spec:loom-harness".into()];
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new(DEFAULT_PROFILE));
    }

    #[test]
    fn resolve_profile_uses_override() {
        let labels = vec!["profile:rust".into()];
        let override_ = ProfileName::new("python");
        let p = resolve_profile(&labels, Some(&override_));
        assert_eq!(p, ProfileName::new("python"));
    }

    #[test]
    fn resolve_profile_first_matching_label_wins() {
        let labels = vec!["profile:rust".into(), "profile:python".into()];
        let p = resolve_profile(&labels, None);
        assert_eq!(p, ProfileName::new("rust"));
    }
}
