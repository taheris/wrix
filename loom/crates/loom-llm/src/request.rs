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
///
/// Omitting the `ModelId` is a compile error:
///
/// ```compile_fail
/// use loom_llm::CompletionRequest;
/// // No `ModelId` argument -> does not compile.
/// let _req = CompletionRequest::new();
/// ```
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheTtl;

    /// `CompletionRequest::new` requires a `ModelId` positionally — the
    /// only public constructor takes one, the `model` field is
    /// non-optional, and there is no `Default` impl. The compile-fail
    /// doctest on the type proves the negative path; this runtime test
    /// pins the positive path (constructed value carries the chosen
    /// model and is reachable from the builder chain).
    #[test]
    fn completion_request_requires_model_at_construction() {
        let req = CompletionRequest::new(ModelId::ClaudeSonnet46)
            .system("prefix")
            .user("question")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .max_tokens(2048);
        assert_eq!(req.model, ModelId::ClaudeSonnet46);
        assert_eq!(req.system.as_deref(), Some("prefix"));
        assert_eq!(req.max_tokens, Some(2048));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content, "question");
        assert!(matches!(req.messages[0].cache, CacheControl::None));
        assert_eq!(req.messages[1].role, Role::User);
        assert!(matches!(
            req.messages[1].cache,
            CacheControl::Ephemeral(CacheTtl::Hours1),
        ));
    }

    /// The builder also chains assistant turns (cached and uncached) —
    /// the surface is symmetric across roles so multi-turn replays can
    /// be reconstructed without dropping into raw `Message` literals.
    #[test]
    fn completion_request_builder_chains_all_roles() {
        let req = CompletionRequest::new(ModelId::Other("gpt-5-preview".to_string()))
            .assistant("previous reply")
            .assistant_cached("cached reply", CacheControl::Ephemeral(CacheTtl::Minutes5));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::Assistant);
        assert!(matches!(req.messages[0].cache, CacheControl::None));
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert!(matches!(
            req.messages[1].cache,
            CacheControl::Ephemeral(CacheTtl::Minutes5),
        ));
    }
}
