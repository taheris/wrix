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
    RuntimeUnavailable,
    RestrictedNetwork,
    IncompatibleImage,
    ApprovalUnavailable,
    ApprovalRequired,
    RepositoryRequired,
}

impl Blocker {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::RuntimeUnavailable => {
                "Shared installed Nix execution is unavailable until packaged scoped runtime acceptance passes; no private fallback is performed."
            }
            Self::RestrictedNetwork => {
                "Shared installed Nix stores require network=open; select private mode for restricted workers."
            }
            Self::IncompatibleImage => {
                "Shared installed Nix stores require a compatible versioned image and seed closure."
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
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NativeAcceptance {
    NotRequested,
    Unverified,
}

#[derive(Debug, Serialize)]
pub(super) struct Admission {
    pub mode: Mode,
    pub native_acceptance: NativeAcceptance,
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
            native_acceptance: NativeAcceptance::NotRequested,
            approval: Approval::NotRequested,
            blockers: Vec::new(),
        });
    }
    // Native source wiring and focused tests do not establish packaged platform acceptance.
    // Keep public Run/Spawn refusal before staging or issuance on every unproved runtime.
    let mut blockers = vec![Blocker::RuntimeUnavailable];
    if network != NetworkMode::Open {
        blockers.push(Blocker::RestrictedNetwork);
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
    Ok(Admission {
        mode,
        native_acceptance: NativeAcceptance::Unverified,
        approval,
        blockers,
    })
}
