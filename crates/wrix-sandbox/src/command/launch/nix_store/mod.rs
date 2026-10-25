//! Shared selection never turns capability metadata or repository edits into admission.

use serde::Serialize;
use wrix_core::{nix_store::Mode, repository_policy::Policy};

use super::{Kind, LaunchError, NetworkMode, Request};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Approval {
    NotRequested,
    Unavailable,
    Unapproved,
    Disabled,
    Enabled,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Blocker {
    RestrictedNetwork,
    IncompatibleImage,
    UnsupportedRuntime,
    ApprovalUnavailable,
    ApprovalRequired,
    RepositoryRequired,
    RuntimeUnavailable,
}

impl Blocker {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::RestrictedNetwork => {
                "Shared installed Nix stores require network=open; select private mode for restricted workers."
            }
            Self::IncompatibleImage => {
                "Shared installed Nix stores require a compatible versioned image and seed closure."
            }
            Self::UnsupportedRuntime => {
                "Shared installed Nix transport is not accepted for this worker runtime; select private mode."
            }
            Self::ApprovalUnavailable => {
                "Shared installed Nix stores require checked host workspace authority; select private mode in a worker."
            }
            Self::ApprovalRequired => {
                "Installed-store sharing requires explicit host membership/approval; use wrix service nix-store enable or select private mode."
            }
            Self::RepositoryRequired => {
                "Shared installed Nix stores require an actual canonical Git repository; select private mode for scratch directories."
            }
            Self::RuntimeUnavailable => {
                "Shared installed Nix execution is unavailable in this version; select private mode until production worker delivery and lifetime acceptance is verified."
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Admission {
    pub mode: Mode,
    pub approval: Approval,
    pub blockers: Vec<Blocker>,
}

pub(super) fn resolve(
    request: &Request,
    policy: &Policy,
    network: NetworkMode,
    selection: &wrix_state::context::Selection,
) -> Result<Admission, LaunchError> {
    let explicit = match &request.kind {
        Kind::Run(run) => run.nix_store,
        Kind::Spawn(spawn) => spawn.config.nix_store,
    };
    let mode = explicit.resolve(policy.nix_store);
    if mode == Mode::Private {
        return Ok(Admission {
            mode,
            approval: Approval::NotRequested,
            blockers: Vec::new(),
        });
    }
    let mut blockers = Vec::new();
    if network != NetworkMode::Open {
        blockers.push(Blocker::RestrictedNetwork);
    }
    if super::Platform::CURRENT != super::Platform::Linux || super::env_flag("WRIX_MICROVM") {
        blockers.push(Blocker::UnsupportedRuntime);
    }
    let immutable_image = request.profile_config.image.digest.is_some()
        && match &request.kind {
            Kind::Run(_) => true,
            Kind::Spawn(spawn) => {
                spawn
                    .config
                    .image_ref
                    .as_ref()
                    .is_none_or(|image| image == &request.profile_config.image.reference)
                    && spawn
                        .config
                        .image_source
                        .as_ref()
                        .is_none_or(|source| source == &request.profile_config.image.source)
            }
        };
    if !immutable_image
        || request
            .profile_config
            .image
            .shared_nix
            .as_ref()
            .is_none_or(|image| image.version.require_supported().is_err())
    {
        blockers.push(Blocker::IncompatibleImage);
    }
    let approval = match wrix_state::nix_store::inspect(selection) {
        Ok(wrix_state::nix_store::Approval::Unavailable) => {
            blockers.push(Blocker::ApprovalUnavailable);
            Approval::Unavailable
        }
        Ok(wrix_state::nix_store::Approval::Unapproved) => {
            blockers.push(Blocker::ApprovalRequired);
            Approval::Unapproved
        }
        Ok(wrix_state::nix_store::Approval::Bound { enabled, .. }) => {
            if enabled {
                Approval::Enabled
            } else {
                blockers.push(Blocker::ApprovalRequired);
                Approval::Disabled
            }
        }
        Err(wrix_state::nix_store::Error::RepositoryRequired) => {
            blockers.push(Blocker::RepositoryRequired);
            Approval::Unavailable
        }
        Err(error) => return Err(error.into()),
    };
    // Metadata approval is not build isolation, resource ownership or export protection.
    blockers.push(Blocker::RuntimeUnavailable);
    Ok(Admission {
        mode,
        approval,
        blockers,
    })
}
