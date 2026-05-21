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
use crate::request::{CompletionRequest, Message};
use crate::tool::{Tool, ToolDef};

/// Behaviour selected by the consumer when the iteration budget is
/// exhausted. Default is [`LoopOutcome::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopOutcome {
    /// Return [`LlmError::IterationBudgetExhausted`].
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
    history: Vec<Message>,
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
            history: Vec::new(),
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

    /// Append a user turn to the conversation history. Subsequent
    /// `run` / `run_stream` calls include it on the next completion.
    pub fn user(&mut self, content: impl Into<String>) {
        self.history.push(Message::user(content));
    }

    /// Run the tool-use loop to completion against `client`. Returns
    /// the final `CompletionResponse` (the assistant turn that did not
    /// emit further tool calls).
    pub async fn run<C: LlmClient + Sync>(
        &mut self,
        client: &C,
    ) -> Result<CompletionResponse, LlmError> {
        let tool_defs: Vec<ToolDef> = self
            .tools
            .iter()
            .map(|tool| ToolDef::from_tool(tool.as_ref()))
            .collect();

        let mut last_response: Option<CompletionResponse> = None;
        let mut iterations: u32 = 0;
        while iterations < self.max_iterations {
            iterations += 1;
            let req = self.build_request(tool_defs.clone());
            let response = client.complete(req).await?;

            if response.tool_calls.is_empty() {
                return Ok(response);
            }

            self.history.push(Message::assistant_tool_use(
                response.text.clone(),
                response.tool_calls.clone(),
            ));

            for call in &response.tool_calls {
                let tool = self
                    .tools
                    .iter()
                    .find(|t| t.name() == call.name)
                    .ok_or_else(|| LlmError::ToolNotRegistered {
                        name: call.name.clone(),
                    })?;
                let output = tool.invoke(call.args.clone()).await?;
                let content = serde_json::to_string(&output.content)?;
                self.history.push(Message::tool_result(
                    call.call_id.clone(),
                    content,
                    output.is_error,
                ));
            }

            last_response = Some(response);
        }

        match self.on_iteration_exhausted {
            LoopOutcome::Error => Err(LlmError::IterationBudgetExhausted {
                budget: self.max_iterations,
            }),
            LoopOutcome::ReturnLast => last_response.ok_or(LlmError::IterationBudgetExhausted {
                budget: self.max_iterations,
            }),
        }
    }

    /// Same as [`Conversation::run`] but yields `AgentEvent` values
    /// during execution so callers can render incremental output.
    /// Wired-up event emission lands with the default sink chain in a
    /// follow-up bead; for now the surface matches `run` so consumers
    /// can adopt the entry point ahead of the streaming work.
    pub async fn run_stream<C: LlmClient + Sync>(
        &mut self,
        client: &C,
    ) -> Result<CompletionResponse, LlmError> {
        self.run(client).await
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

    fn build_request(&self, tool_defs: Vec<ToolDef>) -> CompletionRequest {
        let mut req = CompletionRequest::new(self.model.clone());
        if let Some(prefix) = &self.system {
            req = req.system(prefix.clone());
        }
        for message in &self.history {
            req = req.message(message.clone());
        }
        if !tool_defs.is_empty() {
            req = req.tools(tool_defs);
        }
        req
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{CompletionResponse, ToolUseRequest};
    use crate::request::Role;
    use crate::tool::{InvokeFuture, Tool, ToolOutput};
    use crate::usage::TokenUsage;

    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    struct EchoTool {
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl EchoTool {
        fn new() -> (Self, Arc<Mutex<Vec<Value>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (Self { seen: seen.clone() }, seen)
        }
    }

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo input"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
            let seen = self.seen.clone();
            Box::pin(async move {
                seen.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(args.clone());
                Ok(ToolOutput {
                    content: json!({ "echoed": args }),
                    is_error: false,
                })
            })
        }
    }

    /// Scripted client that returns each scripted response in order so
    /// the loop test can drive multi-iteration flows without a live
    /// provider.
    struct ScriptedClient {
        responses: Mutex<Vec<CompletionResponse>>,
        calls: Arc<Mutex<u32>>,
    }

    impl ScriptedClient {
        fn new(responses: Vec<CompletionResponse>) -> (Self, Arc<Mutex<u32>>) {
            let calls = Arc::new(Mutex::new(0));
            (
                Self {
                    responses: Mutex::new(responses),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    impl LlmClient for ScriptedClient {
        async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
            *self.calls.lock().unwrap_or_else(|p| p.into_inner()) += 1;
            let mut guard = self.responses.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_empty() {
                Err(LlmError::Provider {
                    message: "scripted client out of responses".into(),
                })
            } else {
                Ok(guard.remove(0))
            }
        }

        async fn complete_structured<T>(&self, _req: CompletionRequest) -> Result<T, LlmError>
        where
            T: serde::de::DeserializeOwned + schemars::JsonSchema + Send,
        {
            Err(LlmError::Provider {
                message: "complete_structured not exercised in conversation tests".into(),
            })
        }
    }

    fn no_calls(text: &str) -> CompletionResponse {
        CompletionResponse {
            text: text.to_string(),
            usage: TokenUsage::default(),
            tool_calls: Vec::new(),
        }
    }

    fn with_call(name: &str, call_id: &str, args: Value) -> CompletionResponse {
        CompletionResponse {
            text: String::new(),
            usage: TokenUsage::default(),
            tool_calls: vec![ToolUseRequest {
                call_id: call_id.to_string(),
                name: name.to_string(),
                args,
            }],
        }
    }

    /// `Conversation::new(ModelId)` returns a builder that accepts the
    /// documented knobs (`system`, `register`, `max_iterations`,
    /// `on_iteration_exhausted`) and persists each setting on the
    /// resulting `Conversation` so the loop reads back the same values
    /// the consumer wrote.
    #[test]
    fn conversation_builder_accepts_documented_knobs() {
        let (tool, _seen) = EchoTool::new();
        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .system("be terse")
            .register(tool)
            .max_iterations(7)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("ping");

        assert_eq!(*conv.model(), ModelId::ClaudeSonnet46);
        assert_eq!(conv.max_iterations_value(), 7);
        assert_eq!(conv.on_iteration_exhausted_value(), LoopOutcome::ReturnLast);
        assert_eq!(conv.history.len(), 1);
        assert_eq!(conv.history[0].role, Role::User);
        assert_eq!(conv.history[0].content, "ping");
        assert_eq!(conv.tools.len(), 1);
        assert_eq!(conv.tools[0].name(), "echo");
        assert_eq!(conv.system.as_deref(), Some("be terse"));
    }

    /// The loop dispatches each tool call the model emits, reflects the
    /// tool result back into the next request, and stops at the first
    /// non-tool-calling assistant turn. The returned response is that
    /// final turn.
    #[test]
    fn conversation_run_completes_loop_and_returns_final_response() {
        let (tool, seen) = EchoTool::new();
        let (client, calls) = ScriptedClient::new(vec![
            with_call("echo", "call-1", json!({ "text": "hello" })),
            no_calls("done"),
        ]);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46).register(tool);
        conv.user("do the thing");

        let resp = tokio_test::block_on(conv.run(&client)).expect("run completes");

        assert_eq!(resp.text, "done");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 2);
        let seen_args = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(seen_args, vec![json!({ "text": "hello" })]);

        let history = conv.history.clone();
        let roles: Vec<Role> = history.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(history[1].tool_calls.len(), 1);
        assert_eq!(history[1].tool_calls[0].name, "echo");
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call-1"));
    }

    /// The loop honours `max_iterations`; an unending tool-call stream
    /// terminates with `IterationBudgetExhausted` when
    /// `on_iteration_exhausted` is the default `Error`.
    #[test]
    fn conversation_loop_respects_max_iterations() {
        let (tool, _seen) = EchoTool::new();
        let infinite =
            std::iter::repeat_with(|| with_call("echo", "call", json!({ "text": "again" })))
                .take(32)
                .collect();
        let (client, calls) = ScriptedClient::new(infinite);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .register(tool)
            .max_iterations(3);
        conv.user("loop");

        let err = tokio_test::block_on(conv.run(&client)).expect_err("loop exhausts budget");
        match err {
            LlmError::IterationBudgetExhausted { budget } => assert_eq!(budget, 3),
            other => panic!("expected IterationBudgetExhausted, got {other:?}"),
        }
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 3);
    }

    /// With `LoopOutcome::ReturnLast`, exhausting the budget returns
    /// the last response the loop saw rather than an error.
    #[test]
    fn conversation_loop_return_last_on_exhausted() {
        let (tool, _seen) = EchoTool::new();
        let (client, _calls) = ScriptedClient::new(vec![
            with_call("echo", "call-a", json!({ "text": "1" })),
            with_call("echo", "call-b", json!({ "text": "2" })),
        ]);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .register(tool)
            .max_iterations(2)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("loop");

        let resp = tokio_test::block_on(conv.run(&client)).expect("returns last response");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call-b");
    }

    /// The default `DuplicateResultObserver` ships enabled, and the
    /// builder's `disable_duplicate_result_observer` toggle takes effect
    /// — mirroring `[agent.duplicate_result] enabled = false` in the
    /// CLI-side config.
    #[test]
    fn duplicate_result_config_disable_path() {
        let on = Conversation::new(ModelId::ClaudeSonnet46);
        assert!(on.duplicate_result_enabled());
        let off = Conversation::new(ModelId::ClaudeSonnet46).disable_duplicate_result_observer();
        assert!(!off.duplicate_result_enabled());
    }

    /// Dropping the loop future before it resolves cancels any work
    /// awaiting inside the loop — this is the standard tokio
    /// drop-cancels-future semantics, but the conversation must not
    /// install any guards that fight it. Pinning a future that awaits a
    /// pending tool dispatch and dropping it after one poll proves the
    /// invariant.
    #[test]
    fn conversation_loop_cancellation_aborts_in_flight_work() {
        struct PendingTool;
        impl Tool for PendingTool {
            fn name(&self) -> &str {
                "pending"
            }
            fn description(&self) -> &str {
                "never resolves"
            }
            fn input_schema(&self) -> Value {
                json!({ "type": "object" })
            }
            fn invoke<'a>(&'a self, _args: Value) -> InvokeFuture<'a> {
                Box::pin(std::future::pending())
            }
        }

        let (client, calls) = ScriptedClient::new(vec![with_call("pending", "call-1", json!({}))]);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46).register(PendingTool);
        conv.user("hang");

        let mut fut = Box::pin(conv.run(&client));
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let poll = fut.as_mut().poll(&mut cx);
        assert!(matches!(poll, std::task::Poll::Pending));
        drop(fut);
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 1);
    }
}
