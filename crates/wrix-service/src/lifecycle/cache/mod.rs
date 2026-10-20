//! Host-private issuer bootstrap; backend observations cannot select authority paths.

use std::path::{Component, Path};

use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_cache::backend::{issuer::Authority, layout::Layout};

use super::{Plan, RuntimeStatus};

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// Celler issuer must have a separate absolute host-private location outside the workspace and service mounts
    Location,
    /// {0}
    Layout(#[from] wrix_cache::backend::layout::Error),
    /// {0}
    Issuer(#[from] wrix_cache::backend::issuer::Error),
}

/// Callers hold workspace and host lifecycle locks and have rejected competing backend owners.
/// Running backends require existing private authority; bootstrap never invents its replacement.
pub(super) fn prepare(plan: &Plan, status: RuntimeStatus) -> Result<(), Error> {
    let layout = Layout::current()?;
    let host = plan
        .paths()
        .state_base()
        .and_then(Path::parent)
        .ok_or(Error::Location)?;
    let issuer = host.join("host-control/celler-issuer");
    check_location(&issuer, &layout, plan.workspace().canonical_path())?;
    if status == RuntimeStatus::Running {
        Authority::open(&issuer)?;
    }
    layout.prepare()?;
    Authority::provision(&issuer, layout.metadata())?;
    Ok(())
}

fn check_location(issuer: &Path, layout: &Layout, workspace: &Path) -> Result<(), Error> {
    if !issuer.is_absolute()
        || issuer
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || overlaps(issuer, workspace)
        || layout
            .mounts()
            .iter()
            .any(|mount| overlaps(issuer, mount.source))
    {
        return Err(Error::Location);
    }
    Ok(())
}

fn overlaps(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

#[cfg(test)]
mod test {
    use super::*;
    use wrix_cache::backend::layout::Platform;

    #[test]
    fn authority_cannot_overlap_either_delegated_mount_or_workspace() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::resolve(Platform::Linux, root.path(), None, None).unwrap();
        let workspace = root.path().join("workspace");
        for path in [
            layout.metadata().to_owned(),
            layout.metadata().join("issuer"),
            layout.archives().to_owned(),
            layout.archives().join("issuer"),
            workspace.clone(),
            workspace.join("issuer"),
            root.path().to_owned(),
        ] {
            assert!(matches!(
                check_location(&path, &layout, &workspace),
                Err(Error::Location)
            ));
            assert!(!path.exists() || path == root.path());
        }
        assert!(check_location(&root.path().join("control/issuer"), &layout, &workspace).is_ok());
    }
}
