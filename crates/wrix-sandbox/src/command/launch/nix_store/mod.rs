//! Shared selection never turns capability metadata or repository edits into admission.

use serde::Serialize;
use wrix_core::{nix_store::Mode, repository_policy::Policy};

use super::{Kind, NetworkMode, Request};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Approval {
    NotRequested,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Blocker {
    RestrictedNetwork,
    IncompatibleImage,
    ApprovalUnavailable,
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
            Self::ApprovalUnavailable => {
                "Canonical repository/system pool approval is unavailable in this version; select private mode."
            }
            Self::RuntimeUnavailable => {
                "Shared installed Nix execution is unavailable in this version; select private mode until strict build, protection and export acceptance is implemented."
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

pub(super) fn resolve(request: &Request, policy: &Policy, network: NetworkMode) -> Admission {
    let explicit = match &request.kind {
        Kind::Run(run) => run.nix_store,
        Kind::Spawn(spawn) => spawn.config.nix_store,
    };
    let mode = explicit.resolve(policy.nix_store);
    if mode == Mode::Private {
        return Admission {
            mode,
            approval: Approval::NotRequested,
            blockers: Vec::new(),
        };
    }
    let mut blockers = Vec::new();
    if network != NetworkMode::Open {
        blockers.push(Blocker::RestrictedNetwork);
    }
    if request
        .profile_config
        .image
        .shared_nix
        .as_ref()
        .is_none_or(|image| image.version.require_supported().is_err())
    {
        blockers.push(Blocker::IncompatibleImage);
    }
    // No namespace identity, image declaration or caller setting is pool authority.
    blockers.push(Blocker::ApprovalUnavailable);
    blockers.push(Blocker::RuntimeUnavailable);
    Admission {
        mode,
        approval: Approval::Unavailable,
        blockers,
    }
}
