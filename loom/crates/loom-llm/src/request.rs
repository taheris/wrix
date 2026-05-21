//! `CompletionRequest` — the typed builder consumers compose to call
//! `LlmClient::complete*`. Messages are typed; cache control attaches
//! per-content-block.

use crate::cache::CacheControl;
use crate::model_id::ModelId;

/// One message in a completion request. Constructed via the builder
/// helpers on [`CompletionRequest`]; consumers compose blocks rather
/// than handing in raw JSON.
#[derive(Debug, Clone)]
pub struct Message {
    /// Speaker role.
    pub role: Role,
    /// Free-form text content.
    pub content: String,
    /// Cache-control marker for this content block. Providers that do
    /// not support typed per-block cache markers no-op the marker
    /// without error.
    pub cache: CacheControl,
}

/// Role on a [`Message`]. The system role is carried via
/// [`CompletionRequest::system`] rather than as a `Role` variant so that
/// the system prefix is structurally distinct from the user/assistant
/// turn sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// Typed builder for a single completion. Model is required at
/// construction — `CompletionRequest::new(model)` is the only entry
/// point, so the type system forbids constructing a request without
/// naming the model.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Model the underlying provider should route to.
    pub model: ModelId,
    /// Optional system instruction prefix.
    pub system: Option<String>,
    /// Ordered user/assistant/tool turns.
    pub messages: Vec<Message>,
    /// Optional `max_tokens` cap surfaced through the provider.
    pub max_tokens: Option<u32>,
}

impl CompletionRequest {
    /// Construct a new request. `ModelId` is positional so the type
    /// system requires a model on every call site.
    pub fn new(model: ModelId) -> Self {
        Self {
            model,
            system: None,
            messages: Vec::new(),
            max_tokens: None,
        }
    }

    /// Set the system instruction prefix. Overwrites any prior value.
    pub fn system(mut self, prefix: impl Into<String>) -> Self {
        self.system = Some(prefix.into());
        self
    }

    /// Append a user turn with no cache marker.
    pub fn user(mut self, content: impl Into<String>) -> Self {
        self.messages.push(Message {
            role: Role::User,
            content: content.into(),
            cache: CacheControl::None,
        });
        self
    }

    /// Append a user turn with a per-block cache marker.
    pub fn user_cached(mut self, content: impl Into<String>, cache: CacheControl) -> Self {
        self.messages.push(Message {
            role: Role::User,
            content: content.into(),
            cache,
        });
        self
    }

    /// Append an assistant turn with no cache marker.
    pub fn assistant(mut self, content: impl Into<String>) -> Self {
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.into(),
            cache: CacheControl::None,
        });
        self
    }

    /// Append an assistant turn with a per-block cache marker.
    pub fn assistant_cached(mut self, content: impl Into<String>, cache: CacheControl) -> Self {
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.into(),
            cache,
        });
        self
    }

    /// Cap the provider's response length.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }
}
