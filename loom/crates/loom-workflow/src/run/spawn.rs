use std::path::PathBuf;

use loom_core::agent::{RePinContent, SpawnConfig};

/// Build the [`SpawnConfig`] handed to `wrapix spawn --spawn-config` for a
/// `loom run` bead spawn.
///
/// `image_ref` is the podman ref resolved from the bead's per-bead profile
/// (see [`super::resolve_profile`]); `image_source` is the Nix store path
/// the wrapper hands to `podman load` before invoking podman. The driver
/// maps `ProfileName` → manifest entry before calling — keeping that
/// translation out of this function lets the test suite exercise spawn
/// config shape without a Nix evaluation. `env` is an explicit allowlist;
/// the wrapper never inherits the host environment wholesale.
pub fn build_spawn_config(
    image_ref: String,
    image_source: PathBuf,
    workspace: PathBuf,
    initial_prompt: String,
    repin: RePinContent,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repin() -> RePinContent {
        RePinContent {
            orientation: "ori".into(),
            pinned_context: "ctx".into(),
            partial_bodies: vec![],
        }
    }

    #[test]
    fn spawn_config_carries_prompt_workspace_and_image() {
        let cfg = build_spawn_config(
            "localhost/wrapix-rust:tag".into(),
            PathBuf::from("/nix/store/zzz-wrapix-rust.tar"),
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin(),
            vec![("WRAPIX_AGENT".into(), "claude-code".into())],
            vec!["--print".into()],
        );
        assert_eq!(cfg.image_ref, "localhost/wrapix-rust:tag");
        assert_eq!(
            cfg.image_source,
            PathBuf::from("/nix/store/zzz-wrapix-rust.tar")
        );
        assert_eq!(cfg.workspace, PathBuf::from("/workspace"));
        assert_eq!(cfg.initial_prompt, "PROMPT");
        assert_eq!(
            cfg.env,
            vec![("WRAPIX_AGENT".to_string(), "claude-code".to_string())]
        );
        assert_eq!(cfg.agent_args, vec!["--print".to_string()]);
    }

    #[test]
    fn spawn_config_round_trips_through_json() -> Result<(), serde_json::Error> {
        let cfg = build_spawn_config(
            "localhost/wrapix-rust:tag".into(),
            PathBuf::from("/nix/store/zzz-wrapix-rust.tar"),
            PathBuf::from("/work"),
            "PROMPT".into(),
            repin(),
            vec![],
            vec![],
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
