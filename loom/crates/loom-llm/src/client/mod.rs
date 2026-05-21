//! `LlmClient` — the public-contract trait every backend implements.
//!
//! Per-call model selection (no fixed-model client construction): the
//! same client instance accepts a different model on every request, with
//! `ModelId` carried as a required positional field of
//! [`crate::request::CompletionRequest`]. Provider routing is inferred
//! from the `ModelId` variant (or `Other` prefix).

mod multi_provider;

pub use multi_provider::Client;

use displaydoc::Display;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::request::CompletionRequest;
use crate::usage::TokenUsage;

/// Successful completion outcome. Every call carries token usage so
/// consumers see cache hits and cost directly; the same `TokenUsage` is
/// fanned out as a `DriverKind::TokenUsage` `AgentEvent` for SaaS billing
/// pipelines tailing the live event stream.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// Final assistant text. Tool-use loops yield this from the last
    /// non-tool-calling turn.
    pub text: String,
    /// Per-call token usage including cache fields.
    pub usage: TokenUsage,
}

/// Public-contract error returned by every fallible `loom-llm` surface.
/// Variants are deliberately coarse at the contract layer; provider
/// crates surface their own diagnostic detail in the `source` of
/// `LlmError::Provider`.
#[derive(Debug, Display, Error)]
pub enum LlmError {
    /// not implemented: {what}
    Unimplemented {
        /// Stable identifier for the unimplemented surface (e.g.
        /// `"LlmClient::complete"`). Scaffold-only — concrete impls land
        /// in follow-up beads.
        what: &'static str,
    },
    /// underlying provider failed: {message}
    Provider {
        /// Provider-supplied diagnostic message. Opaque at this layer;
        /// concrete impls map their internal errors into this string.
        message: String,
    },
    /// failed to canonicalize JSON value
    Canonicalize,
    /// failed to deserialize structured response into target type
    Deserialize(#[from] serde_json::Error),
}

/// The public agent-side LLM contract. Per-call model selection;
/// `complete_structured::<T>` hides provider-specific structured-output
/// mechanism behind a single typed method.
pub trait LlmClient: Send + Sync {
    /// Run a completion against the request's `ModelId`. Returns the
    /// final assistant text plus token usage.
    fn complete(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, LlmError>> + Send;

    /// Run a completion that deserializes into `T`. Internally selects
    /// the right provider mechanism (synthetic forced-tool for
    /// Anthropic, `response_format` for OpenAI, `response_schema` for
    /// Gemini) and returns the parsed value.
    fn complete_structured<T>(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<T, LlmError>> + Send
    where
        T: DeserializeOwned + JsonSchema + Send;
}
