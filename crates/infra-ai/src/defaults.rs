//! The single place that names default endpoints and *suggested* models.
//!
//! Model names are never hard-wired into adapters: the profile carries the model the user
//! chose. The suggestions below only pre-fill the profile form and age quickly, so keep them in
//! this file. Endpoints and model names were checked against the providers' documentation on
//! 2026-11-05.

use tan_domain::ProviderKind;

/// Suggested model identifiers per task for one provider (`None`: the provider cannot do it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuggestedModels {
    /// For text generation (summaries).
    pub chat: Option<&'static str>,
    /// For screen-frame understanding.
    pub vision: Option<&'static str>,
    /// For speech-to-text.
    pub transcription: Option<&'static str>,
}

/// Default API root of a provider. Includes the version path where the provider uses one;
/// adapters append only the resource path (`/chat/completions`, `/messages`, ...).
#[must_use]
pub const fn default_base_url(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAi => "https://api.openai.com/v1",
        ProviderKind::Anthropic => "https://api.anthropic.com/v1",
        ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        ProviderKind::DeepSeek => "https://api.deepseek.com",
        ProviderKind::Ollama => "http://localhost:11434",
    }
}

/// Suggested models of a provider.
///
/// `whisper-1` is suggested for `OpenAI` transcription because it is the model that returns
/// segment timestamps (the newer transcription models answer with plain text only). Note that
/// `OpenAI` deprecated it and announced it shuts down on 2027-02-26: revisit this suggestion
/// (and how segment timing is obtained without it) before then.
#[must_use]
pub const fn suggested_models(kind: ProviderKind) -> SuggestedModels {
    match kind {
        ProviderKind::OpenAi => SuggestedModels {
            chat: Some("gpt-5.5"),
            vision: Some("gpt-5.5"),
            transcription: Some("whisper-1"),
        },
        ProviderKind::Anthropic => SuggestedModels {
            chat: Some("claude-sonnet-5-5"),
            vision: Some("claude-sonnet-5-5"),
            transcription: None,
        },
        ProviderKind::Gemini => SuggestedModels {
            chat: Some("gemini-3.8-flash"),
            vision: Some("gemini-3.8-flash"),
            transcription: Some("gemini-3.8-flash"),
        },
        ProviderKind::DeepSeek => SuggestedModels {
            chat: Some("deepseek-flash"),
            vision: None,
            transcription: None,
        },
        ProviderKind::Ollama => SuggestedModels {
            chat: Some("llama3.2"),
            vision: Some("llama3.2-vision"),
            transcription: None,
        },
    }
}

/// `anthropic-version` header value (current version per the API versioning page).
pub const ANTHROPIC_VERSION: &str = "2026-10-21";

/// Reply length bound used when a request names none. Generous because Anthropic requires a
/// value and its models may spend part of it on thinking.
pub const DEFAULT_MAX_TOKENS: u32 = 8_192;

#[cfg(test)]
mod tests {
    use super::{default_base_url, suggested_models};
    use tan_application::ports::Capabilities;
    use tan_domain::{ProviderKind, TaskKind};

    #[test]
    fn suggestions_exist_exactly_where_the_provider_has_the_capability() {
        for &kind in ProviderKind::ALL {
            let caps = Capabilities::for_provider(kind);
            let models = suggested_models(kind);
            assert_eq!(models.chat.is_some(), caps.supports(TaskKind::Summary));
            // Ollama's vision support is a per-profile flag, so a vision model is suggested for
            // it even though the kind-level capability is off.
            assert_eq!(
                models.vision.is_some(),
                caps.supports(TaskKind::Vision) || kind == ProviderKind::Ollama
            );
            assert_eq!(
                models.transcription.is_some(),
                caps.supports(TaskKind::Transcription)
            );
            assert!(!default_base_url(kind).ends_with('/'));
        }
    }
}
