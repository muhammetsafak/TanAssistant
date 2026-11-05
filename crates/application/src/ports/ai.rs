//! AI provider ports (ADR-008): capability-based, not one "LLM" interface.
//!
//! Each capability is its own port ([`ChatModel`], [`VisionModel`], [`TranscriptionModel`]).
//! A [`ProviderFactory`] turns a stored [`ProviderProfile`] into adapters; which capabilities a
//! provider has is a pure fact about its kind ([`Capabilities::for_provider`]) so the use
//! cases can refuse an impossible assignment without any network call.
//!
//! Nothing in this module carries a secret value: adapters receive their key from the factory
//! and every [`AiError`] is safe to show to the user.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use tan_domain::{LanguageCode, ProviderKind, ProviderProfile, TaskKind};

/// What a provider can do (ADR-008 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Text generation.
    pub chat: bool,
    /// Image understanding.
    pub vision: bool,
    /// Speech-to-text.
    pub transcription: bool,
}

impl Capabilities {
    /// The capabilities of a provider kind.
    ///
    /// Ollama's `vision` is `false` here because it depends on the model that is served: use
    /// [`Capabilities::for_profile`], which reads the profile's `supports_vision` flag.
    #[must_use]
    pub const fn for_provider(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAi | ProviderKind::Gemini => Self {
                chat: true,
                vision: true,
                transcription: true,
            },
            ProviderKind::Anthropic => Self {
                chat: true,
                vision: true,
                transcription: false,
            },
            ProviderKind::DeepSeek | ProviderKind::Ollama => Self {
                chat: true,
                vision: false,
                transcription: false,
            },
        }
    }

    /// The capabilities of a configured profile: the provider matrix, except that an Ollama
    /// profile has vision only when it is marked `supports_vision`.
    #[must_use]
    pub const fn for_profile(profile: &ProviderProfile) -> Self {
        let mut caps = Self::for_provider(profile.kind);
        if matches!(profile.kind, ProviderKind::Ollama) {
            caps.vision = profile.supports_vision;
        }
        caps
    }

    /// Whether a provider with these capabilities can perform `task`.
    #[must_use]
    pub const fn supports(self, task: TaskKind) -> bool {
        match task {
            TaskKind::Transcription => self.transcription,
            TaskKind::Vision => self.vision,
            TaskKind::Summary => self.chat,
        }
    }
}

/// Whether the provider needs an API key. Ollama does not (a key is optional there).
#[must_use]
pub const fn requires_api_key(kind: ProviderKind) -> bool {
    !matches!(kind, ProviderKind::Ollama)
}

/// Failure of an AI call. Messages never contain an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// The provider rejected the credentials (HTTP 401/403, invalid key).
    Auth,
    /// The provider is throttling us (HTTP 429).
    RateLimited {
        /// How long the provider asked us to wait, when it said so.
        retry_after: Option<Duration>,
    },
    /// The provider did not answer in time.
    Timeout,
    /// The request was refused as malformed or unsuitable (HTTP 400/413/422, bad input).
    InvalidRequest(String),
    /// The provider (or profile) cannot do what was asked.
    Unsupported(String),
    /// The provider could not be reached.
    Network(String),
    /// The provider answered with another error.
    Provider {
        /// HTTP status.
        status: u16,
        /// Provider message, truncated and redacted.
        message: String,
    },
    /// The API key could not be read from the secret store.
    SecretUnavailable(String),
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth => f.write_str("the provider rejected the API key"),
            Self::RateLimited {
                retry_after: Some(wait),
            } => write!(
                f,
                "the provider is rate limiting requests; retry in {}s",
                wait.as_secs()
            ),
            Self::RateLimited { retry_after: None } => {
                f.write_str("the provider is rate limiting requests")
            }
            Self::Timeout => f.write_str("the provider did not answer in time"),
            Self::InvalidRequest(why) => write!(f, "the provider refused the request: {why}"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
            Self::Network(why) => write!(f, "network error: {why}"),
            Self::Provider { status, message } => {
                write!(f, "provider error (HTTP {status}): {message}")
            }
            Self::SecretUnavailable(why) => write!(f, "the API key is unavailable: {why}"),
        }
    }
}

impl Error for AiError {}

/// Result alias for AI calls.
pub type AiResult<T> = Result<T, AiError>;

/// Who wrote a chat message (the system prompt is a separate field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// The human side.
    User,
    /// The model's earlier replies.
    Assistant,
}

/// One chat turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Author.
    pub role: ChatRole,
    /// Text.
    pub content: String,
}

impl ChatMessage {
    /// A user turn.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }
}

/// A JSON Schema the reply must follow (structured output).
///
/// The schema must be "strict-compatible": every object lists all its properties in
/// `required` and sets `"additionalProperties": false`, and it uses no numeric/string length
/// constraints. That is the common subset `OpenAI`, Anthropic, Gemini and Ollama accept.
/// `DeepSeek` only offers a generic JSON mode, so for it the schema is handed to the model as an
/// instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSchema {
    /// Schema name (letters, digits, `_`, `-`; at most 64 characters).
    pub name: String,
    /// The schema as JSON text.
    pub schema_json: String,
}

/// A text-generation request.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// System prompt.
    pub system: Option<String>,
    /// The conversation so far; the last message should be the user's.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature. Ignored by Anthropic (current API versions removed the parameter).
    pub temperature: Option<f32>,
    /// Upper bound for the reply length; `None` uses a provider-appropriate default.
    pub max_tokens: Option<u32>,
    /// Structured output.
    pub json_schema: Option<JsonSchema>,
}

/// Token accounting, when the provider reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenUsage {
    /// Tokens in the prompt.
    pub input_tokens: u64,
    /// Tokens in the reply.
    pub output_tokens: u64,
}

/// A model reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    /// The reply text (the JSON text for structured output).
    pub text: String,
    /// Token usage, if reported.
    pub usage: Option<TokenUsage>,
    /// The reply hit the length limit and is cut off.
    pub truncated: bool,
}

/// One image sent to a vision model.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageInput {
    /// MIME type, e.g. `image/png`.
    pub mime_type: String,
    /// The encoded image file.
    pub data: Vec<u8>,
}

impl fmt::Debug for ImageInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageInput")
            .field("mime_type", &self.mime_type)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// An image-understanding request: one or more images plus an instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionRequest {
    /// System prompt.
    pub system: Option<String>,
    /// What to do with the images.
    pub prompt: String,
    /// The images (at least one).
    pub images: Vec<ImageInput>,
    /// Sampling temperature (see [`ChatRequest::temperature`]).
    pub temperature: Option<f32>,
    /// Reply length bound.
    pub max_tokens: Option<u32>,
    /// Structured output.
    pub json_schema: Option<JsonSchema>,
}

/// Where the audio to transcribe comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum AudioSource {
    /// A file the adapter reads.
    Path(PathBuf),
    /// Audio already in memory.
    Bytes {
        /// File name including extension (providers sniff the format from it).
        file_name: String,
        /// MIME type, e.g. `audio/wav`.
        mime_type: String,
        /// The encoded audio file.
        data: Vec<u8>,
    },
}

impl fmt::Debug for AudioSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => f.debug_tuple("Path").field(path).finish(),
            Self::Bytes {
                file_name,
                mime_type,
                data,
            } => f
                .debug_struct("Bytes")
                .field("file_name", file_name)
                .field("mime_type", mime_type)
                .field("bytes", &data.len())
                .finish(),
        }
    }
}

/// A speech-to-text request. Cloud providers cap the upload size (`OpenAI` 25 MB, Gemini about
/// 14 MB inline), so long recordings must be split by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionRequest {
    /// The audio.
    pub audio: AudioSource,
    /// Spoken language; `None` lets the provider detect it.
    pub language: Option<LanguageCode>,
    /// Length of the audio, used when the provider returns no timestamps.
    pub duration_ms: Option<u64>,
    /// Vocabulary/context hint (names, jargon), when the provider supports one.
    pub prompt: Option<String>,
}

/// One transcribed utterance as an adapter reports it (the session and track are the
/// pipeline's business, so this is not the domain `TranscriptSegment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSegmentDto {
    /// Start offset from the start of the audio, in milliseconds.
    pub start_ms: u64,
    /// End offset, in milliseconds.
    pub end_ms: u64,
    /// The text, in the spoken language.
    pub text: String,
    /// Detected or forced language, when known.
    pub language: Option<LanguageCode>,
}

/// Text generation.
pub trait ChatModel: Send + Sync {
    /// Generates a reply.
    fn complete(&self, request: ChatRequest)
        -> impl Future<Output = AiResult<ChatResponse>> + Send;
}

/// Image understanding.
pub trait VisionModel: Send + Sync {
    /// Describes / analyses the images according to the prompt.
    fn describe(
        &self,
        request: VisionRequest,
    ) -> impl Future<Output = AiResult<ChatResponse>> + Send;
}

/// Speech-to-text.
pub trait TranscriptionModel: Send + Sync {
    /// Transcribes the audio into time-stamped segments.
    fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> impl Future<Output = AiResult<Vec<TranscriptSegmentDto>>> + Send;
}

/// Lists the models a provider serves (used to test a local Ollama server).
pub trait ModelCatalog: Send + Sync {
    /// The model names.
    fn list_models(&self) -> impl Future<Output = AiResult<Vec<String>>> + Send;
}

/// Builds adapters from stored profiles. The API key is resolved from the secret store here, at
/// the point of use; every method fails with [`AiError::Unsupported`] when the provider lacks
/// the capability.
pub trait ProviderFactory: Send + Sync {
    /// The text-generation adapter type.
    type Chat: ChatModel;
    /// The image-understanding adapter type.
    type Vision: VisionModel;
    /// The speech-to-text adapter type.
    type Transcription: TranscriptionModel;
    /// The model-listing adapter type.
    type Catalog: ModelCatalog;

    /// A chat adapter for `profile`.
    fn chat(&self, profile: &ProviderProfile) -> impl Future<Output = AiResult<Self::Chat>> + Send;

    /// A vision adapter for `profile`.
    fn vision(
        &self,
        profile: &ProviderProfile,
    ) -> impl Future<Output = AiResult<Self::Vision>> + Send;

    /// A transcription adapter for `profile`.
    fn transcription(
        &self,
        profile: &ProviderProfile,
    ) -> impl Future<Output = AiResult<Self::Transcription>> + Send;

    /// A model-listing adapter for `profile`.
    fn catalog(
        &self,
        profile: &ProviderProfile,
    ) -> impl Future<Output = AiResult<Self::Catalog>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{requires_api_key, AiError, Capabilities};
    use tan_domain::{ProviderKind, TaskKind};

    #[test]
    fn capability_table_matches_adr_008() {
        let caps = |k| Capabilities::for_provider(k);
        assert!(caps(ProviderKind::OpenAi).transcription && caps(ProviderKind::OpenAi).vision);
        assert!(caps(ProviderKind::Gemini).transcription && caps(ProviderKind::Gemini).vision);
        assert!(
            caps(ProviderKind::Anthropic).vision && !caps(ProviderKind::Anthropic).transcription
        );
        assert!(
            !caps(ProviderKind::DeepSeek).vision && !caps(ProviderKind::DeepSeek).transcription
        );
        assert!(!caps(ProviderKind::Ollama).transcription);
        for &kind in ProviderKind::ALL {
            assert!(caps(kind).chat);
            assert!(caps(kind).supports(TaskKind::Summary));
        }
    }

    #[test]
    fn only_ollama_works_without_a_key() {
        for &kind in ProviderKind::ALL {
            assert_eq!(requires_api_key(kind), kind != ProviderKind::Ollama);
        }
    }

    #[test]
    fn rate_limit_message_names_the_wait() {
        let err = AiError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(7)),
        };
        assert!(err.to_string().contains("7s"));
    }
}
