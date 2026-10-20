//! Authenticated cache readiness; backend observations cannot select authority paths.

use std::{
    io,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_cache::backend::{
    issuer::Authority,
    layout::Layout,
    publication::{Publisher, registration::provision_private},
};

use super::{Plan, RuntimeStatus};

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// Celler issuer must have a separate absolute host-private location outside the workspace and service mounts
    Location,
    /// {0}
    Layout(#[from] wrix_cache::backend::layout::Error),
    /// {0}
    Issuer(#[from] wrix_cache::backend::issuer::Error),
    /// authenticated Celler namespace readiness failed; runtime startup may have succeeded: {0}
    Registration(#[from] wrix_cache::backend::publication::Error),
    /// cannot initialize authenticated Celler readiness: {0}
    Runtime(#[from] io::Error),
    /// authenticated Celler readiness exceeded its deadline; backend or local registration may have changed, but readiness is unconfirmed
    Deadline,
    /// {0}
    Settings(#[from] wrix_cache::config::ReadError),
}

const DEFAULT_RETENTION: Duration = Duration::from_hours(720);
const READY_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Prepared {
    issuer: PathBuf,
    retention: Duration,
    lock_timeout: Duration,
}

/// Callers hold workspace and host lifecycle locks and have rejected competing backend owners.
/// Running backends require existing private authority; bootstrap never invents its replacement.
pub(super) fn prepare(plan: &Plan, status: RuntimeStatus) -> Result<Prepared, Error> {
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
    let retention =
        match Publisher::retained_policy(plan.paths().state_root(), plan.workspace().hash()) {
            Ok(retention) => retention,
            Err(wrix_cache::backend::publication::Error::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                DEFAULT_RETENTION
            }
            Err(error) => return Err(error.into()),
        };
    let settings = wrix_cache::config::Settings::current()?;
    Ok(Prepared {
        issuer,
        retention,
        lock_timeout: Duration::from_millis(settings.explicit_lock_timeout_ms),
    })
}

/// Runtime liveness alone never admits publication or establishes authenticated readiness.
pub(super) fn activate(plan: &Plan, prepared: &Prepared) -> Result<(), Error> {
    let Some(port) = plan.cache_port() else {
        return Err(Error::Location);
    };
    let endpoint = format!("http://127.0.0.1:{port}/");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(READY_TIMEOUT + prepared.lock_timeout, async {
            loop {
                match provision_private(
                    plan.paths().state_root(),
                    plan.workspace().hash(),
                    &endpoint,
                    &prepared.issuer,
                    prepared.retention,
                    prepared.lock_timeout,
                )
                .await
                {
                    Ok(_) => return Ok(()),
                    Err(wrix_cache::backend::publication::Error::Backend(
                        wrix_cache::backend::Error::Transport,
                    )) => tokio::time::sleep(Duration::from_millis(200)).await,
                    Err(error) => return Err(Error::Registration(error)),
                }
            }
        })
        .await
        .map_err(|_| Error::Deadline)?
    })
}

pub(super) fn retire(plan: &Plan) -> Result<(), Error> {
    let settings = wrix_cache::config::Settings::current()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(wrix_cache::backend::publication::retirement::retire(
        plan.paths().state_root(),
        plan.workspace().hash(),
        Duration::from_millis(settings.explicit_lock_timeout_ms),
    ))?;
    Ok(())
}

pub(super) fn is_retired(plan: &Plan) -> Result<bool, Error> {
    Ok(wrix_cache::backend::publication::retirement::is_retired(
        plan.paths().state_root(),
        plan.workspace().hash(),
    )?)
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
