//! Read-only tool-cache selection and restricted-Rust admission.

use wrix_core::{repository_policy::Policy, tool_cache::Mode};

use super::{Kind, LaunchError, NetworkMode, Request};

pub(super) fn resolve(
    request: &Request,
    policy: &Policy,
    network: NetworkMode,
) -> Result<Mode, LaunchError> {
    let explicit = match &request.kind {
        Kind::Run(run) => run.tool_caches,
        Kind::Spawn(spawn) => spawn.config.tool_caches,
    };
    let mode = Mode::resolve(explicit, policy.tool_caches);
    let profile = &request.profile_config.profile;
    let rust = profile.name.to_string() == "rust"
        || profile.env.keys().any(|name| name.as_str() == "RUSTC");
    if rust && network == NetworkMode::Limit && mode == Mode::Shared {
        return Err(LaunchError::RestrictedRustSharedCaches);
    }
    Ok(mode)
}
