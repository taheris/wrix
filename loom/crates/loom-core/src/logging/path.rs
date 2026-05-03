use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::identifier::{BeadId, SpecLabel};

pub use super::time::format_utc_timestamp;

/// Resolve the per-bead NDJSON log path under
/// `<logs_root>/<spec-label>/<bead-id>-<utc-timestamp>.ndjson`.
///
/// `logs_root` is typically `<workspace>/.wrapix/loom/logs`. Per-bead — not
/// per-session — so concurrent batches never interleave inside a single file
/// (see `specs/loom-harness.md` *Run UX & Logging*).
///
/// The function is pure: it does not create directories or files. Callers
/// (`LogSink::open_in`) handle directory creation.
///
/// ```
/// use loom_core::identifier::{BeadId, SpecLabel};
/// use loom_core::logging::bead_log_path;
/// use std::path::Path;
/// use std::time::{Duration, UNIX_EPOCH};
///
/// let path = bead_log_path(
///     Path::new("/ws/.wrapix/loom/logs"),
///     &SpecLabel::new("loom-harness"),
///     &BeadId::new("wx-3hhwq.9").unwrap(),
///     UNIX_EPOCH + Duration::from_secs(1777811445),
/// );
/// assert_eq!(
///     path,
///     Path::new("/ws/.wrapix/loom/logs/loom-harness/wx-3hhwq.9-20260503T123045Z.ndjson"),
/// );
/// ```
pub fn bead_log_path(
    logs_root: &Path,
    spec_label: &SpecLabel,
    bead_id: &BeadId,
    when: SystemTime,
) -> PathBuf {
    let stamp = format_utc_timestamp(when);
    logs_root
        .join(spec_label.as_str())
        .join(format!("{}-{}.ndjson", bead_id.as_str(), stamp))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn nests_under_spec_label_and_includes_utc_stamp() {
        let path = bead_log_path(
            Path::new("/x/.wrapix/loom/logs"),
            &SpecLabel::new("alpha"),
            &BeadId::new("wx-1").expect("valid bead id"),
            UNIX_EPOCH + Duration::from_secs(0),
        );
        assert_eq!(
            path,
            Path::new("/x/.wrapix/loom/logs/alpha/wx-1-19700101T000000Z.ndjson"),
        );
    }

    #[test]
    fn distinct_spec_labels_yield_distinct_directories() {
        let root = Path::new("/r");
        let when = UNIX_EPOCH + Duration::from_secs(1777811445);
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let p_a = bead_log_path(root, &SpecLabel::new("a"), &bead, when);
        let p_b = bead_log_path(root, &SpecLabel::new("b"), &bead, when);
        assert_ne!(p_a.parent(), p_b.parent());
    }

    #[test]
    fn distinct_beads_in_same_spec_yield_distinct_files() {
        let root = Path::new("/r");
        let when = UNIX_EPOCH + Duration::from_secs(1777811445);
        let label = SpecLabel::new("a");
        let bead_a = BeadId::new("wx-1").expect("valid bead id");
        let bead_b = BeadId::new("wx-2").expect("valid bead id");
        let p_a = bead_log_path(root, &label, &bead_a, when);
        let p_b = bead_log_path(root, &label, &bead_b, when);
        assert_eq!(p_a.parent(), p_b.parent());
        assert_ne!(p_a.file_name(), p_b.file_name());
    }
}
