//! Read-only tool-cache selection and restricted-Rust admission.

use wrix_core::{repository_policy::Policy, tool_cache::Mode};

use super::{Kind, NetworkMode, Request};

pub(super) fn resolve(request: &Request, policy: &Policy) -> Mode {
    let explicit = match &request.kind {
        Kind::Run(run) => run.tool_caches,
        Kind::Spawn(spawn) => spawn.config.tool_caches,
    };
    Mode::resolve(explicit, policy.tool_caches)
}

pub(super) fn restricted_conflict(request: &Request, network: NetworkMode, mode: Mode) -> bool {
    let profile = &request.profile_config.profile;
    let rust = profile.name.to_string() == "rust"
        || profile.env.keys().any(|name| name.as_str() == "RUSTC");
    rust && network == NetworkMode::Limit && mode == Mode::Shared
}
