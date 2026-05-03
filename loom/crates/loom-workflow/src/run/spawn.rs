use std::path::PathBuf;

use loom_core::agent::{RePinContent, SpawnConfig};

/// Build the [`SpawnConfig`] handed to `wrapix run-bead --spawn-config` for a
/// `loom run` bead spawn.
///
/// `image` is the wrapix container reference resolved from the bead's
/// per-bead profile (see [`super::resolve_profile`]). The driver maps
/// `ProfileName` → image path before calling — keeping that translation out
/// of this function lets the test suite exercise spawn config shape without
/// a Nix evaluation. `env` is an explicit allowlist; the wrapper never
/// inherits the host environment wholesale.
pub fn build_spawn_config(
    image: String,
    workspace: PathBuf,
    initial_prompt: String,
    repin: RePinContent,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
) -> SpawnConfig {
    SpawnConfig {
        image,
        workspace,
        env: extra_env,
        initial_prompt,
        agent_args,
        repin,
        model: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin(),
            vec![("WRAPIX_AGENT".into(), "claude-code".into())],
            vec!["--print".into()],
        );
        assert_eq!(cfg.image, "localhost/wrapix-rust:tag");
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
            PathBuf::from("/work"),
            "PROMPT".into(),
            repin(),
            vec![],
            vec![],
        );
        let json = serde_json::to_string(&cfg)?;
        let decoded: SpawnConfig = serde_json::from_str(&json)?;
        assert_eq!(decoded.image, cfg.image);
        assert_eq!(decoded.workspace, cfg.workspace);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        Ok(())
    }
}
