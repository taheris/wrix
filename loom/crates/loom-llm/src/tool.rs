//! `Tool` — the handler abstraction for `Conversation`'s built-in
//! tool-use loop. The trait shape is designed for reasonable conversion
//! to other ecosystem agent-loop crates' tool surfaces.

use std::pin::Pin;

use serde_json::Value;

use crate::client::LlmError;

/// Boxed future returned from [`Tool::invoke`]. Boxed so `dyn Tool` is
/// dyn-compatible — concrete handlers compose into the loop's
/// `Vec<Box<dyn Tool>>` registry without per-type monomorphisation.
pub type InvokeFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, LlmError>> + Send + 'a>>;

/// The result a `Tool` returns from `invoke`. Carries a canonical JSON
/// payload the loop forwards to the agent as a `tool_result` block; the
/// payload is also the input to result-hashing observers
/// (`DoomLoopObserver`, `DuplicateResultObserver`).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Canonical result payload. Observers hash the canonical-JSON form;
    /// the loop forwards it to the agent unmodified.
    pub content: Value,
    /// When true, the loop reports this result to the agent as an
    /// error tool-result; the agent typically retries or steers away.
    pub is_error: bool,
}

/// Handler trait every consumer-registered tool implements. The shape
/// (name + description + JSON-Schema input + async invoke) is
/// reasonably convertible to other Rust agent-loop crates' tool shapes
/// — keeps the option of re-hosting `Conversation` on a different
/// agent-loop crate later without breaking consumers.
pub trait Tool: Send + Sync {
    /// Stable tool name advertised to the model. Matches the JSON-Schema
    /// tool identifier the model echoes in `tool_use` blocks.
    fn name(&self) -> &str;

    /// Human-readable description the model consults to decide whether
    /// to call this tool.
    fn description(&self) -> &str;

    /// JSON-Schema describing the tool's accepted arguments.
    fn input_schema(&self) -> Value;

    /// Run the tool against the model-supplied arguments and return a
    /// canonical result payload (or an error). Returns a boxed future
    /// so the trait is dyn-compatible — the loop holds handlers as
    /// `Vec<Box<dyn Tool>>`.
    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a>;
}
