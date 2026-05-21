//! `ModelId` — typed enum of known LLM models with an `Other(String)`
//! forward-compat fallback. Provider routing is inferred from the
//! variant; `Other` routes via prefix match on the carried string.

/// One LLM model that `LlmClient` knows how to route. Adding a known
/// model is a minor version bump; removing or renaming a variant is a
/// major bump (RS-17 closed-set discipline with `Other` for wire-additive
/// growth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelId {
    // Anthropic — Claude family
    ClaudeOpus47,
    ClaudeSonnet46,
    ClaudeHaiku45,
    // OpenAI — GPT family
    Gpt4o,
    Gpt4oMini,
    // Google — Gemini family
    Gemini25Pro,
    Gemini25Flash,
    /// Forward-compat fallback for fine-tuned / custom / not-yet-known
    /// models. Routing falls back to a provider-prefix match on the
    /// carried string (e.g. `"claude-*"` -> Anthropic, `"gpt-*"` ->
    /// OpenAI).
    Other(String),
}

/// Coarse provider routing inferred from a `ModelId`. Concrete
/// implementations of `LlmClient` consult `provider()` to dispatch each
/// call to the right backend; the routing rules live alongside the
/// `ModelId` enum so adding a model variant is a single-file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Google,
    /// Provider could not be inferred from the carried model string.
    /// Concrete impls return a typed error rather than guessing.
    Unknown,
}

impl ModelId {
    /// Provider this model routes to.
    pub fn provider(&self) -> Provider {
        match self {
            ModelId::ClaudeOpus47 | ModelId::ClaudeSonnet46 | ModelId::ClaudeHaiku45 => {
                Provider::Anthropic
            }
            ModelId::Gpt4o | ModelId::Gpt4oMini => Provider::OpenAi,
            ModelId::Gemini25Pro | ModelId::Gemini25Flash => Provider::Google,
            ModelId::Other(s) => provider_from_prefix(s),
        }
    }
}

fn provider_from_prefix(s: &str) -> Provider {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("claude") {
        Provider::Anthropic
    } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
        Provider::OpenAi
    } else if lower.starts_with("gemini") {
        Provider::Google
    } else {
        Provider::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known variants route to their owning provider.
    #[test]
    fn modelid_known_variants_route_to_expected_provider() {
        let cases = [
            (ModelId::ClaudeOpus47, Provider::Anthropic),
            (ModelId::ClaudeSonnet46, Provider::Anthropic),
            (ModelId::ClaudeHaiku45, Provider::Anthropic),
            (ModelId::Gpt4o, Provider::OpenAi),
            (ModelId::Gpt4oMini, Provider::OpenAi),
            (ModelId::Gemini25Pro, Provider::Google),
            (ModelId::Gemini25Flash, Provider::Google),
        ];
        for (model, expected) in cases {
            assert_eq!(model.provider(), expected, "model {model:?}");
        }
    }

    /// Spec contract: `ModelId::Other(String)` routes provider via prefix
    /// match on the carried string — `claude-*` -> Anthropic, `gpt-*` /
    /// `o1-*` / `o3-*` -> OpenAi, `gemini-*` -> Google. Strings that
    /// don't match a known prefix fall through to `Provider::Unknown` so
    /// concrete `LlmClient` impls surface a typed error rather than
    /// guessing.
    #[test]
    fn modelid_other_fallback_routes_provider_by_prefix() {
        let cases = [
            ("claude-3-7-sonnet-future", Provider::Anthropic),
            ("Claude-Opus-Custom", Provider::Anthropic),
            ("gpt-5-preview", Provider::OpenAi),
            ("GPT-5", Provider::OpenAi),
            ("o1-mini", Provider::OpenAi),
            ("o3-pro", Provider::OpenAi),
            ("gemini-3-ultra", Provider::Google),
            ("Gemini-Flash-Lite", Provider::Google),
            ("llama-3-70b", Provider::Unknown),
            ("mistral-large", Provider::Unknown),
            ("", Provider::Unknown),
        ];
        for (carried, expected) in cases {
            let routed = ModelId::Other(carried.to_string()).provider();
            assert_eq!(routed, expected, "prefix routing for {carried:?}");
        }
    }
}
