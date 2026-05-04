use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::error::ProtocolError;
use super::repin::RePinContent;
use super::session::{AgentSession, Idle};

/// Configuration `loom` hands to `wrapix run-bead` describing how to launch
/// the per-bead container and what initial agent state to install.
///
/// Serialized to a JSON file (`/tmp/loom-<id>.json`) and read back by
/// `wrapix run-bead --spawn-config <file>` — this is the single stable
/// boundary between loom and the wrapper. `env` is an explicit allowlist;
/// the wrapper never inherits the host environment wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    /// Podman image reference (e.g. `localhost/wrapix-rust:<hash>`) — the
    /// argument passed to `podman run`. Populated by loom from the
    /// profile-image manifest at dispatch time.
    pub image_ref: String,
    /// Nix store path to a `podman load`-compatible archive that materializes
    /// `image_ref`. The wrapper runs `podman load < image_source` before
    /// `podman run`; the load is idempotent on the ref's hash tag.
    pub image_source: PathBuf,
    pub workspace: PathBuf,
    pub env: Vec<(String, String)>,
    pub initial_prompt: String,
    pub agent_args: Vec<String>,
    pub repin: RePinContent,
    /// Optional post-spawn model override consumed by the host-side backend
    /// (currently only [`PiBackend`](crate::agent::AgentBackend) — claude
    /// receives its model via CLI flags). When present, the pi backend sends
    /// a `set_model` RPC after the startup probe; failure is hard-fail.
    /// Skipped during serialization when `None` so the wrapper's input JSON
    /// remains identical to existing fixtures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
}

/// Per-session model override: pi RPC's `set_model { provider, modelId }`.
///
/// Lives on [`SpawnConfig`] rather than a backend-specific config object so
/// the [`AgentBackend::spawn`] trait surface stays a single-argument call.
/// The wrapper ignores this field; it is consumed only by host-side backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

/// Outcome of a completed agent session — what the workflow engine receives
/// after the session reaches `SessionComplete`.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
}

/// Backend abstraction: spawn a session and return it in the `Idle` state.
///
/// The trait surface is deliberately minimal — process lifecycle only.
/// Conversation driving (prompt, steer, abort, event streaming) lives on
/// [`AgentSession`] so both backends share one concrete session type.
///
/// `async fn` in traits is used directly (no `async-trait`) — backends are
/// zero-sized types dispatched via a type parameter (`<B: AgentBackend>`),
/// so the compiler monomorphizes per concrete backend at each call site.
/// The desugared `impl Future + Send` form pins the auto-trait bound so the
/// returned future can cross task boundaries in `loom-workflow`.
pub trait AgentBackend: Send + Sync {
    fn spawn(
        config: &SpawnConfig,
    ) -> impl std::future::Future<Output = Result<AgentSession<Idle>, ProtocolError>> + Send;
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use crate::agent::repin::RePinContent;

    fn sample_config(model: Option<ModelSelection>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrapix-test:tag".into(),
            image_source: PathBuf::from("/nix/store/zzz-wrapix-test.tar"),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRAPIX_AGENT".into(), "pi".into())],
            initial_prompt: "hello".into(),
            agent_args: vec!["--print".into()],
            repin: RePinContent {
                orientation: "ori".into(),
                pinned_context: "pc".into(),
                partial_bodies: vec![],
            },
            model,
        }
    }

    /// `model: None` is omitted from the on-disk JSON via
    /// `#[serde(skip_serializing_if = "Option::is_none")]`. Wrappers that
    /// pre-date the field added in wx-pkht8.* must continue to round-trip
    /// the serialized fixture identically — the absence of `model` proves
    /// the no-drift contract.
    #[test]
    fn spawn_config_with_model_none_omits_model_key() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("model"),
            "model: None must be omitted, got JSON: {json}"
        );
        // Seven top-level keys remain — any silent rename or drop fails here.
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        for required in [
            "image_ref",
            "image_source",
            "workspace",
            "env",
            "initial_prompt",
            "agent_args",
            "repin",
        ] {
            assert!(keys.contains(&required), "missing key {required}: {json}");
        }
    }

    /// `model: Some(_)` round-trips with both `provider` and `model_id`
    /// reaching the deserialized struct. Pin both field names so the
    /// pi `set_model` RPC stays correct end-to-end.
    #[test]
    fn spawn_config_with_model_some_round_trips_both_fields() {
        let cfg = sample_config(Some(ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        }));
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        let model = back.model.expect("model present");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.model_id, "deepseek-v3");
    }

    /// JSON without a `model` key still parses (treated as `None`) — this is
    /// the contract with wrappers built before wx-pkht8.* landed.
    #[test]
    fn spawn_config_legacy_fixture_without_model_key_parses() {
        let legacy = r#"{
            "image_ref": "localhost/img:tag",
            "image_source": "/nix/store/zzz-img.tar",
            "workspace": "/workspace",
            "env": [["A","1"]],
            "initial_prompt": "go",
            "agent_args": [],
            "repin": {"orientation":"o","pinned_context":"p","partial_bodies":[]}
        }"#;
        let cfg: SpawnConfig = serde_json::from_str(legacy).expect("legacy fixture parses");
        assert!(cfg.model.is_none());
        assert_eq!(cfg.image_ref, "localhost/img:tag");
        assert_eq!(cfg.image_source, PathBuf::from("/nix/store/zzz-img.tar"));
        assert_eq!(cfg.env, vec![("A".to_string(), "1".to_string())]);
    }
}
