use std::path::PathBuf;

use loom_core::agent::{RePinContent, SpawnConfig};
use loom_core::identifier::ProfileName;

/// Build the [`SpawnConfig`] handed to `wrapix spawn --spawn-config` for a
/// `loom todo` session.
///
/// `loom todo` runs **before** beads exist for the spec, so there is no per-
/// bead `profile:X` label to read. The driver supplies the configured profile
/// (typically `base`, overridable via `[phase.todo] profile` in
/// `.wrapix/loom/config.toml`). `image_ref` is the podman ref resolved from
/// that profile; `image_source` is the Nix store path the wrapper hands to
/// `podman load`.
#[expect(
    clippy::too_many_arguments,
    reason = "explicit field-by-field builder mirrors SpawnConfig's wire shape"
)]
pub fn build_spawn_config(
    image_ref: String,
    image_source: PathBuf,
    workspace: PathBuf,
    initial_prompt: String,
    repin: RePinContent,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
    _profile: ProfileName,
) -> SpawnConfig {
    SpawnConfig {
        image_ref,
        image_source,
        workspace,
        env: extra_env,
        initial_prompt,
        agent_args,
        repin,
        model: None,
        shutdown_grace: None,
        handshake_timeout: None,
        stall_warn_interval: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_config_carries_prompt_and_workspace() {
        let repin = RePinContent {
            orientation: "ori".into(),
            pinned_context: "ctx".into(),
            partial_bodies: vec![],
        };
        let cfg = build_spawn_config(
            "wrapix-base:latest".into(),
            PathBuf::from("/nix/store/zzz-wrapix-base.tar"),
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin,
            vec![("FOO".into(), "bar".into())],
            vec!["--print".into()],
            ProfileName::new("base"),
        );
        assert_eq!(cfg.image_ref, "wrapix-base:latest");
        assert_eq!(
            cfg.image_source,
            PathBuf::from("/nix/store/zzz-wrapix-base.tar")
        );
        assert_eq!(cfg.workspace, PathBuf::from("/workspace"));
        assert_eq!(cfg.initial_prompt, "PROMPT");
        assert_eq!(cfg.env, vec![("FOO".to_string(), "bar".to_string())]);
        assert_eq!(cfg.agent_args, vec!["--print".to_string()]);
    }

    #[test]
    fn spawn_config_round_trips_through_json() -> Result<(), serde_json::Error> {
        let repin = RePinContent {
            orientation: "ori".into(),
            pinned_context: "ctx".into(),
            partial_bodies: vec!["partial".into()],
        };
        let cfg = build_spawn_config(
            "wrapix-base:latest".into(),
            PathBuf::from("/nix/store/zzz-wrapix-base.tar"),
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin,
            vec![],
            vec![],
            ProfileName::new("base"),
        );
        let json = serde_json::to_string(&cfg)?;
        let decoded: SpawnConfig = serde_json::from_str(&json)?;
        assert_eq!(decoded.image_ref, cfg.image_ref);
        assert_eq!(decoded.image_source, cfg.image_source);
        assert_eq!(decoded.workspace, cfg.workspace);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        Ok(())
    }
}
