//! `Conversation` — multi-turn builder + built-in tool-use loop.
//!
//! Consumers register handlers via the [`Tool`](crate::tool::Tool)
//! trait, configure budget and exhaustion behaviour, then call
//! [`Conversation::run`] (fire-and-forget) or
//! [`Conversation::run_stream`] (event stream). The loop iterates
//! `complete -> tool_calls? -> dispatch -> tool_results -> complete`
//! until the agent stops calling tools or the iteration budget is
//! exhausted.

use crate::client::{CompletionResponse, LlmClient, LlmError};
use crate::model_id::ModelId;
use crate::request::{Message, Role};
use crate::tool::Tool;

/// Behaviour selected by the consumer when the iteration budget is
/// exhausted. Default is [`LoopOutcome::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopOutcome {
    /// Return [`LlmError::Provider`] indicating the budget was hit.
    #[default]
    Error,
    /// Return the last `CompletionResponse` produced before the cap.
    ReturnLast,
}

/// Multi-turn conversation with built-in tool-use loop.
pub struct Conversation {
    model: ModelId,
    system: Option<String>,
    tools: Vec<Box<dyn Tool>>,
    max_iterations: u32,
    on_iteration_exhausted: LoopOutcome,
    pending: Vec<Message>,
    doom_loop_enabled: bool,
    duplicate_result_enabled: bool,
}

impl Conversation {
    /// Construct a new conversation rooted at the named model. Per-call
    /// model override happens by re-issuing requests with a different
    /// `ModelId`; the conversation's `ModelId` is the default for
    /// `complete` calls the loop emits.
    pub fn new(model: ModelId) -> Self {
        Self {
            model,
            system: None,
            tools: Vec::new(),
            max_iterations: 50,
            on_iteration_exhausted: LoopOutcome::Error,
            pending: Vec::new(),
            doom_loop_enabled: true,
            duplicate_result_enabled: true,
        }
    }

    /// Set the system instruction prefix the loop carries on every
    /// underlying `complete` call.
    pub fn system(mut self, prefix: impl Into<String>) -> Self {
        self.system = Some(prefix.into());
        self
    }

    /// Register a tool handler. Order of registration is preserved; the
    /// loop dispatches by tool name on each model-issued `tool_use`.
    pub fn register(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Box::new(tool));
        self
    }

    /// Cap iterations the loop runs before applying
    /// [`Conversation::on_iteration_exhausted`].
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Behaviour when the iteration cap is hit without the agent
    /// stopping.
    pub fn on_iteration_exhausted(mut self, outcome: LoopOutcome) -> Self {
        self.on_iteration_exhausted = outcome;
        self
    }

    /// Disable the default `DoomLoopObserver`. The observer ships
    /// enabled-by-default; this knob mirrors the
    /// `[agent.doom_loop] enabled = false` config used by the binary.
    pub fn disable_doom_loop_observer(mut self) -> Self {
        self.doom_loop_enabled = false;
        self
    }

    /// Disable the default `DuplicateResultObserver`. Mirrors
    /// `[agent.duplicate_result] enabled = false` in the binary config.
    pub fn disable_duplicate_result_observer(mut self) -> Self {
        self.duplicate_result_enabled = false;
        self
    }

    /// Append a user turn to the pending message buffer. Subsequent
    /// `run` / `run_stream` calls consume the buffer.
    pub fn user(&mut self, content: impl Into<String>) {
        self.pending.push(Message {
            role: Role::User,
            content: content.into(),
            cache: crate::cache::CacheControl::None,
        });
    }

    /// Run the tool-use loop to completion against `client`. Returns
    /// the final `CompletionResponse` (the assistant turn that did not
    /// emit further tool calls).
    pub async fn run<C: LlmClient + Sync>(
        &mut self,
        _client: &C,
    ) -> Result<CompletionResponse, LlmError> {
        Err(LlmError::Unimplemented {
            what: "Conversation::run",
        })
    }

    /// Same as [`Conversation::run`] but yields `AgentEvent` values
    /// during execution so callers can render incremental output.
    pub async fn run_stream<C: LlmClient + Sync>(
        &mut self,
        _client: &C,
    ) -> Result<CompletionResponse, LlmError> {
        Err(LlmError::Unimplemented {
            what: "Conversation::run_stream",
        })
    }

    /// Read-only view of the conversation's current `ModelId`.
    pub fn model(&self) -> &ModelId {
        &self.model
    }

    /// Read-only view of the iteration budget.
    pub fn max_iterations_value(&self) -> u32 {
        self.max_iterations
    }

    /// Read-only view of the exhaustion behaviour.
    pub fn on_iteration_exhausted_value(&self) -> LoopOutcome {
        self.on_iteration_exhausted
    }

    /// Whether the default `DoomLoopObserver` is enabled for this
    /// conversation.
    pub fn doom_loop_enabled(&self) -> bool {
        self.doom_loop_enabled
    }

    /// Whether the default `DuplicateResultObserver` is enabled for
    /// this conversation.
    pub fn duplicate_result_enabled(&self) -> bool {
        self.duplicate_result_enabled
    }
}
