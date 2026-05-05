use std::path::PathBuf;

use loom_core::agent::{RePinContent, SpawnConfig};
use loom_core::bd::Bead;
use loom_core::identifier::ProfileName;
use loom_core::profile_manifest::{ProfileError, ProfileImageManifest};

use super::profile::resolve_profile_image;

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
        shutdown_grace: None,
    }
}

/// Build a [`SpawnConfig`] for `bead` by resolving its profile through the
/// parsed [`ProfileImageManifest`].
///
/// Implements `specs/loom-harness.md` § Profile-Image Manifest per-bead
/// dispatch: the bead's `profile:X` label (or the CLI `--profile` override)
/// is looked up against the manifest to fill `image_ref` + `image_source`.
/// Missing manifest entries surface as [`ProfileError::UnknownProfile`] so
/// the dispatcher can fail loudly instead of falling back to a default
/// profile silently. `phase_default` carries the per-phase fallback name
/// (already chained through `[phase.run]` → `[phase.default]` → built-in
/// `base` by `LoomConfig::agent_for`).
#[expect(clippy::too_many_arguments, reason = "explicit dispatch surface")]
pub fn build_spawn_config_from_manifest(
    manifest: &ProfileImageManifest,
    bead: &Bead,
    override_: Option<&ProfileName>,
    phase_default: &ProfileName,
    workspace: PathBuf,
    initial_prompt: String,
    repin: RePinContent,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
) -> Result<SpawnConfig, ProfileError> {
    let entry = resolve_profile_image(manifest, &bead.labels, override_, phase_default)?;
    Ok(build_spawn_config(
        entry.r#ref.clone(),
        entry.source.clone(),
        workspace,
        initial_prompt,
        repin,
        extra_env,
        agent_args,
    ))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_core::bd::Label;
    use loom_core::identifier::BeadId;

    fn repin() -> RePinContent {
        RePinContent {
            orientation: "ori".into(),
            pinned_context: "ctx".into(),
            partial_bodies: vec![],
        }
    }

    fn bead_with_labels(id: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title-{id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
        }
    }

    fn three_profile_manifest(dir: &std::path::Path) -> ProfileImageManifest {
        let body = r#"{
          "base":   { "ref": "localhost/wrapix-base:abc",   "source": "/nix/store/aaa-image-base" },
          "rust":   { "ref": "localhost/wrapix-rust:def",   "source": "/nix/store/bbb-image-rust" },
          "python": { "ref": "localhost/wrapix-python:ghi", "source": "/nix/store/ccc-image-python" }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        ProfileImageManifest::from_path(&path).expect("parse manifest")
    }

    fn base() -> ProfileName {
        ProfileName::new("base")
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

    /// Per-bead dispatch: two beads with different `profile:X` labels
    /// produce SpawnConfigs with different `image_ref` + `image_source`.
    /// Implements `tests/loom-test.sh::test_per_bead_profile_spawn` (Rust
    /// side — argv-shape verified by the integration test in
    /// `loom/tests/spawn_dispatch.rs`).
    #[test]
    fn per_bead_profile_dispatch_produces_distinct_image_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());

        let rust_bead = bead_with_labels("wx-1", &["spec:loom-harness", "profile:rust"]);
        let python_bead = bead_with_labels("wx-2", &["spec:loom-harness", "profile:python"]);

        let cfg_rust = build_spawn_config_from_manifest(
            &manifest,
            &rust_bead,
            None,
            &base(),
            PathBuf::from("/work/wx-1"),
            "rust prompt".into(),
            repin(),
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let cfg_python = build_spawn_config_from_manifest(
            &manifest,
            &python_bead,
            None,
            &base(),
            PathBuf::from("/work/wx-2"),
            "python prompt".into(),
            repin(),
            vec![],
            vec![],
        )
        .expect("python dispatch");

        assert_eq!(cfg_rust.image_ref, "localhost/wrapix-rust:def");
        assert_eq!(
            cfg_rust.image_source,
            PathBuf::from("/nix/store/bbb-image-rust")
        );
        assert_eq!(cfg_python.image_ref, "localhost/wrapix-python:ghi");
        assert_eq!(
            cfg_python.image_source,
            PathBuf::from("/nix/store/ccc-image-python")
        );
        assert_ne!(cfg_rust.image_ref, cfg_python.image_ref);
        assert_ne!(cfg_rust.image_source, cfg_python.image_source);
    }

    /// FR5 (`--profile` CLI override precedence): the same bead resolves
    /// to two different SpawnConfigs depending on whether the override is
    /// applied. Implements `tests/loom-test.sh::test_profile_cli_override`.
    #[test]
    fn cli_override_swaps_resolved_image() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["spec:loom-harness", "profile:rust"]);

        let labelled = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/wx-1"),
            "p".into(),
            repin(),
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let overridden = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            Some(&ProfileName::new("python")),
            &base(),
            PathBuf::from("/work/wx-1"),
            "p".into(),
            repin(),
            vec![],
            vec![],
        )
        .expect("python dispatch");

        assert_eq!(labelled.image_ref, "localhost/wrapix-rust:def");
        assert_eq!(overridden.image_ref, "localhost/wrapix-python:ghi");
        assert_ne!(labelled.image_ref, overridden.image_ref);
    }

    /// wx-cmzob: sequential (`loom run`) and parallel (`loom run -p N`)
    /// must produce identical SpawnConfigs for the same bead modulo the
    /// workspace path — sequential dispatches against the repo root,
    /// parallel against a per-bead worktree, but every other field
    /// (image_ref, image_source, env, agent_args, prompt, repin) must
    /// match. If either path adds an arg or rewrites the prompt format,
    /// this test trips before the divergence reaches users.
    #[test]
    fn sequential_and_parallel_dispatch_produce_identical_spawn_configs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["spec:loom-harness", "profile:rust"]);
        let prompt = format!("loom run: bead {}", bead.id);

        let seq = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/repo-root"),
            prompt.clone(),
            repin(),
            vec![],
            vec![],
        )
        .expect("sequential dispatch");
        let par = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/repo-root/.wrapix/worktree/wx-1"),
            prompt,
            repin(),
            vec![],
            vec![],
        )
        .expect("parallel dispatch");

        assert_eq!(seq.image_ref, par.image_ref);
        assert_eq!(seq.image_source, par.image_source);
        assert_eq!(seq.env, par.env);
        assert_eq!(seq.agent_args, par.agent_args);
        assert_eq!(seq.initial_prompt, par.initial_prompt);
        assert!(seq.model.is_none() && par.model.is_none());
        assert_ne!(
            seq.workspace, par.workspace,
            "workspace MUST differ — parallel uses a per-bead worktree",
        );
    }

    /// A bead with a `profile:X` not declared in the manifest fails
    /// loudly with [`ProfileError::UnknownProfile`] — no silent default.
    #[test]
    fn unknown_profile_label_returns_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["profile:ruby"]);

        let err = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work"),
            "p".into(),
            repin(),
            vec![],
            vec![],
        )
        .expect_err("expected unknown profile");
        match err {
            ProfileError::UnknownProfile { name, .. } => {
                assert_eq!(name, ProfileName::new("ruby"));
            }
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }
}
