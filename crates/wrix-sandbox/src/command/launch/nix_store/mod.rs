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
    LinuxPodman,
    LinuxKrun,
}

/// Metadata cannot qualify a host/runtime pair without actual packaged acceptance.
fn acceptance(platform: super::Platform, architecture: &str, microvm: bool) -> NativeAcceptance {
    match (platform, architecture, microvm) {
        (super::Platform::Linux, "x86_64", false) => NativeAcceptance::LinuxPodman,
        (super::Platform::Linux, "x86_64", true) => NativeAcceptance::LinuxKrun,
        _ => NativeAcceptance::Unverified,
    }
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
    let native_acceptance = acceptance(
        super::Platform::CURRENT,
        std::env::consts::ARCH,
        super::env_flag("WRIX_MICROVM"),
    );
    let mut blockers = Vec::new();
    if matches!(native_acceptance, NativeAcceptance::Unverified) {
        blockers.push(Blocker::RuntimeUnavailable);
    }
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
        native_acceptance,
        approval,
        blockers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qualification_is_bound_to_actual_linux_architecture_and_runtime() {
        for platform in [
            super::super::Platform::Linux,
            super::super::Platform::Darwin,
        ] {
            for architecture in ["x86_64", "aarch64", "unknown"] {
                for microvm in [false, true] {
                    let qualified = !matches!(
                        acceptance(platform, architecture, microvm),
                        NativeAcceptance::Unverified
                    );
                    assert_eq!(
                        qualified,
                        platform == super::super::Platform::Linux && architecture == "x86_64"
                    );
                }
            }
        }
    }
}
