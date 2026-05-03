//! Compile-only test pinning static dispatch over both backends.
//!
//! The verify shell function runs `cargo build --workspace --tests`; this
//! file failing to compile is the assertion. The local `run_agent` helper
//! mirrors the dispatch shape that lives in `loom-workflow` (and the spec's
//! Architecture section): a generic free function over `<B: AgentBackend>`
//! that the binary monomorphizes once per concrete backend.

use loom_agent::{ClaudeBackend, PiBackend};
use loom_core::agent::{AgentBackend, ProtocolError, SessionOutcome, SpawnConfig};

async fn run_agent<B: AgentBackend>(config: &SpawnConfig) -> Result<SessionOutcome, ProtocolError> {
    let _session = B::spawn(config).await?;
    Err(ProtocolError::Unsupported)
}

#[test]
fn pi_and_claude_dispatch_through_run_agent() {
    // The bound `B: AgentBackend` is the dispatch contract — instantiating
    // it at both concrete types is what monomorphizes `run_agent` and proves
    // the trait surface accepts each backend.
    fn assert_backend<B: AgentBackend>() {}
    assert_backend::<PiBackend>();
    assert_backend::<ClaudeBackend>();

    // Reference the generic function at each backend so the test binary
    // pulls in both `run_agent::<PiBackend>` and `run_agent::<ClaudeBackend>`
    // monomorphizations rather than only the trait-bound check above.
    let _pi_fut = async {
        let cfg = sample_config();
        run_agent::<PiBackend>(&cfg).await
    };
    let _claude_fut = async {
        let cfg = sample_config();
        run_agent::<ClaudeBackend>(&cfg).await
    };
}

fn sample_config() -> SpawnConfig {
    use loom_core::agent::RePinContent;
    SpawnConfig {
        image: String::new(),
        workspace: std::path::PathBuf::new(),
        env: Vec::new(),
        initial_prompt: String::new(),
        agent_args: Vec::new(),
        repin: RePinContent {
            orientation: String::new(),
            pinned_context: String::new(),
            partial_bodies: Vec::new(),
        },
        model: None,
    }
}
