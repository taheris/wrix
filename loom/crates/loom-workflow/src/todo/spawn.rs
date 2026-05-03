use std::path::PathBuf;

use loom_core::agent::{RePinContent, SpawnConfig};
use loom_core::identifier::ProfileName;

/// Build the [`SpawnConfig`] handed to `wrapix run-bead --spawn-config` for a
/// `loom todo` session.
///
/// `loom todo` runs **before** beads exist for the spec, so there is no per-
/// bead `profile:X` label to read. The driver supplies the configured profile
/// (typically `base`, overridable via `LoomConfig.agent.todo`). The image
/// argument is the wrapix container reference resolved from that profile.
pub fn build_spawn_config(
    image: String,
    workspace: PathBuf,
    initial_prompt: String,
    repin: RePinContent,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
    _profile: ProfileName,
) -> SpawnConfig {
    SpawnConfig {
        image,
        workspace,
        env: extra_env,
        initial_prompt,
        agent_args,
        repin,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin,
            vec![("FOO".into(), "bar".into())],
            vec!["--print".into()],
            ProfileName::new("base"),
        );
        assert_eq!(cfg.image, "wrapix-base:latest");
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
            PathBuf::from("/workspace"),
            "PROMPT".into(),
            repin,
            vec![],
            vec![],
            ProfileName::new("base"),
        );
        let json = serde_json::to_string(&cfg)?;
        let decoded: SpawnConfig = serde_json::from_str(&json)?;
        assert_eq!(decoded.image, cfg.image);
        assert_eq!(decoded.workspace, cfg.workspace);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        Ok(())
    }
}
