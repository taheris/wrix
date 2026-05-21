//! Concrete `LlmClient` impl on top of the multi-provider `genai` crate.
//!
//! The wrapper insulates consumers from `genai`'s API churn: every
//! consumer-facing type is defined inside `loom-llm`, and swapping
//! `genai` for another underlying crate is an internal change with no
//! breaking surface impact.

use genai::chat::{
    CacheControl as GenAiCacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse,
    ChatRole, MessageContent, MessageOptions, Usage as GenAiUsage,
};

use crate::cache::{CacheControl, CacheTtl};
use crate::client::{CompletionResponse, LlmClient, LlmError};
use crate::model_id::ModelId;
use crate::request::{CompletionRequest, Message, Role};
use crate::usage::TokenUsage;

/// Concrete `LlmClient` over the underlying multi-provider crate. The
/// struct carries no model — every `complete*` call routes to the
/// `ModelId` named on the request, so a single instance fans out across
/// providers and per-call model variants.
#[derive(Debug, Clone, Default)]
pub struct Client {
    inner: genai::Client,
}

impl Client {
    /// Construct a client using the underlying crate's default
    /// configuration. Anthropic credentials are read from
    /// `ANTHROPIC_API_KEY`; other providers use their respective env
    /// vars per the underlying crate's resolvers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a client around an explicit underlying `genai::Client`.
    /// Useful when consumers want to inject a pre-configured HTTP client,
    /// custom auth resolver, or alternate endpoints.
    pub fn from_genai(inner: genai::Client) -> Self {
        Self { inner }
    }
}

impl LlmClient for Client {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let model_name = model_id_to_provider_name(&req.model);
        let (chat_req, options) = to_genai_chat_request(req);
        let resp = self
            .inner
            .exec_chat(model_name, chat_req, Some(&options))
            .await
            .map_err(|err| LlmError::Provider {
                message: err.to_string(),
            })?;
        Ok(from_genai_chat_response(resp))
    }

    async fn complete_structured<T>(&self, _req: CompletionRequest) -> Result<T, LlmError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema + Send,
    {
        Err(LlmError::Unimplemented {
            what: "LlmClient::complete_structured",
        })
    }
}

/// Map a typed [`ModelId`] to the underlying crate's model-name string.
/// Known variants resolve to documented provider model IDs; `Other(s)`
/// passes the carried string through so consumers can name fine-tuned
/// or not-yet-listed models without waiting for a `ModelId` minor bump.
pub(crate) fn model_id_to_provider_name(model: &ModelId) -> String {
    match model {
        ModelId::ClaudeOpus47 => "claude-opus-4-5".to_string(),
        ModelId::ClaudeSonnet46 => "claude-sonnet-4-5".to_string(),
        ModelId::ClaudeHaiku45 => "claude-haiku-4-5".to_string(),
        ModelId::Gpt4o => "gpt-4o".to_string(),
        ModelId::Gpt4oMini => "gpt-4o-mini".to_string(),
        ModelId::Gemini25Pro => "gemini-2.5-pro".to_string(),
        ModelId::Gemini25Flash => "gemini-2.5-flash".to_string(),
        ModelId::Other(s) => s.clone(),
    }
}

pub(crate) fn to_genai_chat_request(req: CompletionRequest) -> (ChatRequest, ChatOptions) {
    let CompletionRequest {
        model: _,
        system,
        messages,
        max_tokens,
    } = req;

    let chat_messages: Vec<ChatMessage> = messages.into_iter().map(to_chat_message).collect();

    let mut chat_req = ChatRequest::new(chat_messages);
    if let Some(prefix) = system {
        chat_req = chat_req.with_system(prefix);
    }

    let mut options = ChatOptions::default();
    if let Some(n) = max_tokens {
        options.max_tokens = Some(n);
    }

    (chat_req, options)
}

fn to_chat_message(msg: Message) -> ChatMessage {
    let role = match msg.role {
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
        Role::Tool => ChatRole::Tool,
    };

    let mut chat_msg = ChatMessage::new(role, MessageContent::from_text(msg.content));
    if let Some(cache) = cache_control_to_genai(&msg.cache) {
        let opts = MessageOptions::default().with_cache_control(cache);
        chat_msg = chat_msg.with_options(opts);
    }
    chat_msg
}

pub(crate) fn cache_control_to_genai(cache: &CacheControl) -> Option<GenAiCacheControl> {
    match cache {
        CacheControl::None => None,
        CacheControl::Ephemeral(CacheTtl::Minutes5) => Some(GenAiCacheControl::Ephemeral5m),
        CacheControl::Ephemeral(CacheTtl::Hours1) => Some(GenAiCacheControl::Ephemeral1h),
        CacheControl::Ephemeral(CacheTtl::Hours24) => Some(GenAiCacheControl::Ephemeral24h),
    }
}

pub(crate) fn from_genai_chat_response(resp: ChatResponse) -> CompletionResponse {
    let usage = from_genai_usage(&resp.usage);
    let text = resp.into_first_text().unwrap_or_default();
    CompletionResponse { text, usage }
}

fn from_genai_usage(usage: &GenAiUsage) -> TokenUsage {
    let input = usage.prompt_tokens.unwrap_or(0).max(0) as u32;
    let output = usage.completion_tokens.unwrap_or(0).max(0) as u32;
    let (cache_read, cache_write) =
        usage
            .prompt_tokens_details
            .as_ref()
            .map_or((0_u32, 0_u32), |details| {
                (
                    details.cached_tokens.unwrap_or(0).max(0) as u32,
                    details.cache_creation_tokens.unwrap_or(0).max(0) as u32,
                )
            });
    TokenUsage {
        input,
        output,
        cache_read,
        cache_write,
        cost_cents: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheTtl;
    use genai::ModelIden;
    use genai::adapter::AdapterKind;
    use genai::chat::{PromptTokensDetails, Usage as GenAiUsage};

    /// `loom_llm::Client` is `Send + Sync + 'static` — required for the
    /// `LlmClient: Send + Sync` bound and for spawning across tokio
    /// tasks.
    #[test]
    fn client_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<Client>();
    }

    /// `Client` impls `LlmClient` — the trait bound is satisfied at the
    /// type-check level. The dummy generic forces monomorphization
    /// against the concrete type.
    #[test]
    fn client_impls_llm_client() {
        fn assert_impl<T: LlmClient>() {}
        assert_impl::<Client>();
    }

    /// Known `ModelId` variants resolve to the documented provider
    /// model strings; the `Other(String)` arm passes through so
    /// consumers can name not-yet-listed models without a minor bump.
    #[test]
    fn model_id_to_provider_name_maps_known_and_other_variants() {
        assert_eq!(
            model_id_to_provider_name(&ModelId::ClaudeOpus47),
            "claude-opus-4-5"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::ClaudeSonnet46),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::ClaudeHaiku45),
            "claude-haiku-4-5"
        );
        assert_eq!(model_id_to_provider_name(&ModelId::Gpt4o), "gpt-4o");
        assert_eq!(
            model_id_to_provider_name(&ModelId::Gpt4oMini),
            "gpt-4o-mini"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Gemini25Pro),
            "gemini-2.5-pro"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Gemini25Flash),
            "gemini-2.5-flash"
        );
        let custom = "claude-3-7-sonnet-future";
        assert_eq!(
            model_id_to_provider_name(&ModelId::Other(custom.to_string())),
            custom,
        );
    }

    /// Every documented `CacheTtl` maps to the matching genai variant;
    /// `CacheControl::None` lowers to no `MessageOptions` so the wire
    /// payload remains pristine when no cache breakpoint is requested.
    #[test]
    fn cache_control_lowers_to_matching_genai_variant() {
        assert_eq!(cache_control_to_genai(&CacheControl::None), None);
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Minutes5)),
            Some(GenAiCacheControl::Ephemeral5m),
        );
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Hours1)),
            Some(GenAiCacheControl::Ephemeral1h),
        );
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Hours24)),
            Some(GenAiCacheControl::Ephemeral24h),
        );
    }

    /// Cache markers land on the matching message in the lowered
    /// `ChatRequest`, so the Anthropic adapter places the cache
    /// breakpoint at the per-content-block position the consumer
    /// chose. Adjacent uncached messages carry no `MessageOptions` so
    /// the wire payload reflects the per-block precision the typed
    /// surface promises.
    #[test]
    fn message_text_cached_marks_per_block_in_anthropic_request() {
        let req = CompletionRequest::new(ModelId::ClaudeSonnet46)
            .system("be terse")
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .max_tokens(512);

        let (chat_req, options) = to_genai_chat_request(req);

        assert_eq!(chat_req.system.as_deref(), Some("be terse"));
        assert_eq!(chat_req.messages.len(), 2);
        assert_eq!(chat_req.messages[0].role, ChatRole::User);
        assert!(chat_req.messages[0].options.is_none());
        assert_eq!(chat_req.messages[1].role, ChatRole::User);
        let cache = chat_req.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref())
            .expect("second message carries cache_control");
        assert_eq!(cache, &GenAiCacheControl::Ephemeral1h);
        assert_eq!(options.max_tokens, Some(512));
    }

    /// Lowering a cache-marked request when the request targets an
    /// OpenAI model is a no-op error path: the marker is carried into
    /// the lowered representation (the underlying adapter is free to
    /// drop it), and the conversion never fails. The wrapper does not
    /// silently strip the marker either — the per-provider decision
    /// lives in the underlying adapter, not in this crate.
    #[test]
    fn cache_marker_no_ops_on_openai_provider() {
        let req = CompletionRequest::new(ModelId::Gpt4o)
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Minutes5));

        let (chat_req, _) = to_genai_chat_request(req);
        assert_eq!(chat_req.messages.len(), 2);
        let cache = chat_req.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref())
            .expect("cache marker is preserved through lowering");
        assert_eq!(cache, &GenAiCacheControl::Ephemeral5m);
    }

    /// `ChatResponse → CompletionResponse` carries the final text and
    /// the full `TokenUsage` quintuple: `input`, `output`, `cache_read`,
    /// `cache_write`, `cost_cents`. The cache fields are pulled from
    /// the provider's prompt-tokens detail block.
    #[test]
    fn from_genai_chat_response_extracts_text_and_cache_usage() {
        let model = ModelIden::new(AdapterKind::Anthropic, "claude-sonnet-4-5");
        let usage = GenAiUsage {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(250),
            total_tokens: Some(1_250),
            prompt_tokens_details: Some(PromptTokensDetails {
                cache_creation_tokens: Some(400),
                cached_tokens: Some(600),
                ..PromptTokensDetails::default()
            }),
            completion_tokens_details: None,
        };
        let resp = ChatResponse {
            content: MessageContent::from_text("the answer"),
            reasoning_content: None,
            model_iden: model.clone(),
            provider_model_iden: model,
            stop_reason: None,
            usage,
            captured_raw_body: None,
            response_id: None,
        };

        let completion = from_genai_chat_response(resp);
        assert_eq!(completion.text, "the answer");
        assert_eq!(completion.usage.input, 1_000);
        assert_eq!(completion.usage.output, 250);
        assert_eq!(completion.usage.cache_read, 600);
        assert_eq!(completion.usage.cache_write, 400);
        assert_eq!(completion.usage.cost_cents, 0);
    }

    /// `complete_structured` returns `LlmError::Unimplemented` until
    /// B.6 wires up the per-provider mechanism. The variant identifies
    /// the surface so observability can distinguish the scaffold stub
    /// from a runtime provider failure.
    #[test]
    fn complete_structured_returns_unimplemented() {
        let client = Client::new();
        let req = CompletionRequest::new(ModelId::ClaudeSonnet46).user("anything");
        let result: Result<serde_json::Value, _> =
            tokio_test::block_on(client.complete_structured(req));
        match result {
            Err(LlmError::Unimplemented { what }) => {
                assert_eq!(what, "LlmClient::complete_structured");
            }
            other => panic!("expected Unimplemented, got {other:?}"),
        }
    }
}
