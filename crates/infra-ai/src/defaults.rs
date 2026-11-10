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
        // A CLI provider has no endpoint (ADR-013).
        ProviderKind::ClaudeCli | ProviderKind::CodexCli => "",
    }
}

/// Suggested models of a provider.
///
/// `OpenAI` transcription (checked against the API reference and the deprecations page on
/// 2026-11-06): `gpt-transcribe` is the model `OpenAI` recommends. It, like `gpt-4o-mini-transcribe`
/// and `gpt-4o-transcribe`, returns text without segment timestamps; the use case sends short
/// pieces so each still gets a time. Only `whisper-1` returns segment timestamps itself. `OpenAI`
/// announced that `whisper-1`, `gpt-4o-transcribe` and `gpt-4o-mini-transcribe` shut down on
/// 2027-02-26, so the default is the model that stays; the shutdowns themselves are listed by
/// [`deprecated_models`].
#[must_use]
pub const fn suggested_models(kind: ProviderKind) -> SuggestedModels {
    match kind {
        ProviderKind::OpenAi => SuggestedModels {
            chat: Some("gpt-5.5"),
            vision: Some("gpt-5.5"),
            transcription: Some("gpt-transcribe"),
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
        // `claude --model` takes aliases; `sonnet` always means the latest Sonnet.
        ProviderKind::ClaudeCli => SuggestedModels {
            chat: Some("sonnet"),
            vision: Some("sonnet"),
            transcription: None,
        },
        // Taken from the Codex models page (learn.chatgpt.com/docs/models, 2026-11-10); not
        // tried against a real `codex` because none was installed.
        ProviderKind::CodexCli => SuggestedModels {
            chat: Some("gpt-6.1-sol"),
            vision: Some("gpt-6.1-sol"),
            transcription: None,
        },
    }
}

/// A model its provider has announced it will switch off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelDeprecation {
    /// The model identifier, as a profile stores it.
    pub model: &'static str,
    /// The day the provider shuts the model down (`YYYY-MM-DD`).
    pub shutdown_date: &'static str,
    /// The model the provider recommends instead, when it names one.
    pub replacement: Option<&'static str>,
}

/// The `OpenAI` models that are scheduled for shutdown. Checked against `OpenAI`'s deprecations
/// page on 2026-11-06 (announcements of 2026-07-20 and 2026-08-26). This is the only place that
/// knows about deprecations; the IPC layer and the UI read it.
const OPENAI_DEPRECATED: [ModelDeprecation; 5] = [
    ModelDeprecation {
        model: "whisper-1",
        shutdown_date: "2027-02-26",
        replacement: Some("gpt-transcribe"),
    },
    ModelDeprecation {
        model: "gpt-4o-transcribe",
        shutdown_date: "2027-02-26",
        replacement: Some("gpt-transcribe"),
    },
    ModelDeprecation {
        model: "gpt-4o-mini-transcribe",
        shutdown_date: "2027-02-26",
        replacement: Some("gpt-transcribe"),
    },
    ModelDeprecation {
        model: "gpt-4o-transcribe-diarize",
        shutdown_date: "2027-02-26",
        replacement: Some("gpt-transcribe"),
    },
    ModelDeprecation {
        model: "gpt-4o-mini-transcribe-2025-03-20",
        shutdown_date: "2027-01-20",
        replacement: Some("gpt-4o-mini-transcribe-2025-12-15"),
    },
];

/// The models of a provider that are scheduled for shutdown (empty for providers whose
/// announcements have not been recorded here).
#[must_use]
pub fn deprecated_models(kind: ProviderKind) -> &'static [ModelDeprecation] {
    match kind {
        ProviderKind::OpenAi => &OPENAI_DEPRECATED,
        ProviderKind::Anthropic
        | ProviderKind::Gemini
        | ProviderKind::DeepSeek
        | ProviderKind::Ollama
        | ProviderKind::ClaudeCli
        | ProviderKind::CodexCli => &[],
    }
}

/// `anthropic-version` header value (current version per the API versioning page).
pub const ANTHROPIC_VERSION: &str = "2026-10-21";

/// Reply length bound used when a request names none. Generous because Anthropic requires a
/// value and its models may spend part of it on thinking.
pub const DEFAULT_MAX_TOKENS: u32 = 8_192;

#[cfg(test)]
mod tests {
    use super::{default_base_url, deprecated_models, suggested_models, ModelDeprecation};
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
            assert_eq!(
                default_base_url(kind).is_empty(),
                tan_application::ports::is_cli_provider(kind),
                "only the CLI providers have no endpoint"
            );
        }
    }

    /// The notice of `model` the way a caller would look it up: an exact name match.
    fn notice_of(kind: ProviderKind, model: &str) -> Option<&'static ModelDeprecation> {
        deprecated_models(kind).iter().find(|d| d.model == model)
    }

    #[test]
    fn the_suggested_openai_model_is_one_that_stays_and_the_shutdowns_are_listed() {
        let suggested = suggested_models(ProviderKind::OpenAi)
            .transcription
            .unwrap();
        assert!(notice_of(ProviderKind::OpenAi, suggested).is_none());
        for id in [
            "whisper-1",
            "gpt-4o-transcribe",
            "gpt-4o-mini-transcribe",
            "gpt-4o-transcribe-diarize",
        ] {
            let notice = notice_of(ProviderKind::OpenAi, id).unwrap();
            assert_eq!(notice.shutdown_date, "2027-02-26", "{id}");
            assert_eq!(notice.replacement, Some("gpt-transcribe"), "{id}");
        }
        assert_eq!(
            notice_of(ProviderKind::OpenAi, "gpt-4o-mini-transcribe-2025-03-20")
                .unwrap()
                .shutdown_date,
            "2027-01-20"
        );
        // Exact names only; other providers have none recorded.
        assert!(notice_of(ProviderKind::OpenAi, "whisper").is_none());
        assert!(notice_of(ProviderKind::Gemini, "whisper-1").is_none());
        for &kind in ProviderKind::ALL {
            for notice in deprecated_models(kind) {
                // Dates are plain ISO `YYYY-MM-DD`.
                let parts: Vec<_> = notice.shutdown_date.split('-').collect();
                assert_eq!(parts.len(), 3, "{}", notice.model);
                assert!(parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2);
            }
        }
    }
}
