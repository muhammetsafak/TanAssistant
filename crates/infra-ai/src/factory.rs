//! [`AiProviderFactory`]: turns a stored [`ProviderProfile`] into a ready adapter.
//!
//! The API key is read from the secret store here, at the point of use, through
//! [`ResolveSecret`] and handed to the adapter, which keeps it in a redacting wrapper.

use std::sync::Arc;

use tan_application::credentials::{CredentialsError, ResolveSecret};
use tan_application::ports::{
    is_cli_provider, requires_api_key, AiError, AiResult, Capabilities, ChatModel, ChatRequest,
    ChatResponse, ModelCatalog, ProviderFactory, SecretMetadataRepository, SecretStore,
    SecretValue, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest, VisionModel,
    VisionRequest,
};
use tan_domain::{ProviderKind, ProviderProfile, TaskKind};

use crate::anthropic::AnthropicClient;
use crate::claude_cli::ClaudeCliClient;
use crate::cli::CliSettings;
use crate::codex_cli::CodexCliClient;
use crate::deepseek::DeepSeekClient;
use crate::defaults::default_base_url;
use crate::gemini::GeminiClient;
use crate::http::{ClientConfig, HttpClient, HttpSettings};
use crate::ollama::OllamaClient;
use crate::openai::OpenAiClient;

/// Any provider adapter. It implements every capability trait; calling one the provider lacks
/// returns [`AiError::Unsupported`] (the factory never hands out such a call, because it checks
/// [`Capabilities`] first).
#[derive(Debug)]
pub enum AiProvider {
    /// `OpenAI`.
    OpenAi(OpenAiClient),
    /// Anthropic.
    Anthropic(AnthropicClient),
    /// Gemini.
    Gemini(GeminiClient),
    /// `DeepSeek`.
    DeepSeek(DeepSeekClient),
    /// Ollama.
    Ollama(OllamaClient),
    /// The Claude Code CLI (ADR-013).
    ClaudeCli(ClaudeCliClient),
    /// The Codex CLI (ADR-013).
    CodexCli(CodexCliClient),
}

impl AiProvider {
    /// Builds the adapter for `kind`. A CLI kind only uses the model of `config` (and the
    /// default [`CliSettings`]);
    #[must_use]
    pub fn new(kind: ProviderKind, http: HttpClient, config: ClientConfig) -> Self {
        match kind {
            ProviderKind::ClaudeCli | ProviderKind::CodexCli => {
                Self::new_cli(kind, config.model, CliSettings::default())
            }
            ProviderKind::OpenAi => Self::OpenAi(OpenAiClient::new(http, config)),
            ProviderKind::Anthropic => Self::Anthropic(AnthropicClient::new(http, config)),
            ProviderKind::Gemini => Self::Gemini(GeminiClient::new(http, config)),
            ProviderKind::DeepSeek => Self::DeepSeek(DeepSeekClient::new(http, config)),
            ProviderKind::Ollama => Self::Ollama(OllamaClient::new(http, config)),
        }
    }

    /// Builds the adapter of a CLI provider kind (it finds the binary at call time). Only
    /// called with a kind for which [`is_cli_provider`] holds.
    #[must_use]
    pub(crate) fn new_cli(kind: ProviderKind, model: String, settings: CliSettings) -> Self {
        match kind {
            ProviderKind::ClaudeCli => Self::ClaudeCli(ClaudeCliClient::new(model, settings)),
            ProviderKind::CodexCli => Self::CodexCli(CodexCliClient::new(model, settings)),
            other => panic!("{other} is not a CLI provider"),
        }
    }

    fn kind(&self) -> ProviderKind {
        match self {
            Self::OpenAi(_) => ProviderKind::OpenAi,
            Self::Anthropic(_) => ProviderKind::Anthropic,
            Self::Gemini(_) => ProviderKind::Gemini,
            Self::DeepSeek(_) => ProviderKind::DeepSeek,
            Self::Ollama(_) => ProviderKind::Ollama,
            Self::ClaudeCli(_) => ProviderKind::ClaudeCli,
            Self::CodexCli(_) => ProviderKind::CodexCli,
        }
    }

    fn unsupported(&self, what: &str) -> AiError {
        AiError::Unsupported(format!("{} does not offer {what}", self.kind()))
    }
}

impl ChatModel for AiProvider {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        match self {
            Self::OpenAi(c) => c.complete(request).await,
            Self::Anthropic(c) => c.complete(request).await,
            Self::Gemini(c) => c.complete(request).await,
            Self::DeepSeek(c) => c.complete(request).await,
            Self::Ollama(c) => c.complete(request).await,
            Self::ClaudeCli(c) => c.complete(request).await,
            Self::CodexCli(c) => c.complete(request).await,
        }
    }
}

impl VisionModel for AiProvider {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        match self {
            Self::OpenAi(c) => c.describe(request).await,
            Self::Anthropic(c) => c.describe(request).await,
            Self::Gemini(c) => c.describe(request).await,
            Self::Ollama(c) => c.describe(request).await,
            Self::ClaudeCli(c) => c.describe(request).await,
            Self::CodexCli(c) => c.describe(request).await,
            Self::DeepSeek(_) => Err(self.unsupported("image understanding")),
        }
    }
}

impl TranscriptionModel for AiProvider {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        match self {
            Self::OpenAi(c) => c.transcribe(request).await,
            Self::Gemini(c) => c.transcribe(request).await,
            Self::Anthropic(_)
            | Self::DeepSeek(_)
            | Self::Ollama(_)
            | Self::ClaudeCli(_)
            | Self::CodexCli(_) => Err(self.unsupported("transcription")),
        }
    }
}

impl ModelCatalog for AiProvider {
    async fn list_models(&self) -> AiResult<Vec<String>> {
        match self {
            Self::Ollama(c) => c.list_models().await,
            _ => Err(self.unsupported("a model listing")),
        }
    }
}

/// Builds [`AiProvider`]s, resolving API keys through [`ResolveSecret`].
pub struct AiProviderFactory<M, S> {
    secrets: Arc<ResolveSecret<M, S>>,
    http: HttpClient,
    cli: CliSettings,
}

impl<M, S> AiProviderFactory<M, S>
where
    M: SecretMetadataRepository,
    S: SecretStore,
{
    /// Creates the factory with the default timeouts and retry policy.
    pub fn new(secrets: Arc<ResolveSecret<M, S>>) -> AiResult<Self> {
        Self::with_settings(secrets, HttpSettings::default())
    }

    /// Creates the factory with custom HTTP settings.
    pub fn with_settings(
        secrets: Arc<ResolveSecret<M, S>>,
        settings: HttpSettings,
    ) -> AiResult<Self> {
        Ok(Self {
            secrets,
            http: HttpClient::new(settings)?,
            cli: CliSettings::default(),
        })
    }

    /// Replaces the settings of the CLI providers (the call timeout).
    #[must_use]
    pub fn with_cli_settings(mut self, cli: CliSettings) -> Self {
        self.cli = cli;
        self
    }

    async fn build(&self, profile: &ProviderProfile, task: TaskKind) -> AiResult<AiProvider> {
        if !Capabilities::for_profile(profile).supports(task) {
            return Err(AiError::Unsupported(format!(
                "{} cannot be used for the {task} task",
                profile.kind
            )));
        }
        if is_cli_provider(profile.kind) {
            // No secret to resolve and no endpoint to build (ADR-013). The profile use case
            // refuses both for these kinds; a hand-edited database must not smuggle them in.
            if profile.secret_ref.is_some() || profile.base_url.is_some() {
                return Err(AiError::InvalidRequest(format!(
                    "{} takes neither an API key nor a base URL",
                    profile.kind
                )));
            }
            return Ok(AiProvider::new_cli(
                profile.kind,
                profile.model.clone(),
                self.cli,
            ));
        }
        let api_key = self.resolve_key(profile).await?;
        let base_url = profile
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url(profile.kind).to_owned());
        // Defence in depth (the profile use case enforces the same rule): never put a key on
        // an unencrypted connection to another machine.
        if api_key.is_some() && is_plain_http_to_remote_host(&base_url) {
            return Err(AiError::InvalidRequest(
                "refusing to send the API key over plain http to a non-local host".to_owned(),
            ));
        }
        Ok(AiProvider::new(
            profile.kind,
            self.http.clone(),
            ClientConfig::new(base_url, profile.model.clone(), api_key),
        ))
    }

    async fn resolve_key(&self, profile: &ProviderProfile) -> AiResult<Option<SecretValue>> {
        let Some(secret_id) = &profile.secret_ref else {
            return if requires_api_key(profile.kind) {
                Err(AiError::SecretUnavailable(
                    "the profile has no API key secret".to_owned(),
                ))
            } else {
                Ok(None)
            };
        };
        match self.secrets.execute(secret_id).await {
            Ok(value) => Ok(Some(value)),
            // Ollama works without a key; a dangling reference there is not fatal.
            Err(CredentialsError::NotFound { .. } | CredentialsError::SecretValueMissing)
                if !requires_api_key(profile.kind) =>
            {
                Ok(None)
            }
            Err(error) => Err(AiError::SecretUnavailable(error.to_string())),
        }
    }
}

/// `http://` to anything but localhost / a loopback address.
fn is_plain_http_to_remote_host(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    !(host == "localhost" || host == "127.0.0.1" || host == "::1")
}

impl<M, S> ProviderFactory for AiProviderFactory<M, S>
where
    M: SecretMetadataRepository,
    S: SecretStore,
{
    type Chat = AiProvider;
    type Vision = AiProvider;
    type Transcription = AiProvider;
    type Catalog = AiProvider;

    async fn chat(&self, profile: &ProviderProfile) -> AiResult<AiProvider> {
        self.build(profile, TaskKind::Summary).await
    }

    async fn vision(&self, profile: &ProviderProfile) -> AiResult<AiProvider> {
        self.build(profile, TaskKind::Vision).await
    }

    async fn transcription(&self, profile: &ProviderProfile) -> AiResult<AiProvider> {
        self.build(profile, TaskKind::Transcription).await
    }

    async fn catalog(&self, profile: &ProviderProfile) -> AiResult<AiProvider> {
        if profile.kind != ProviderKind::Ollama {
            return Err(AiError::Unsupported(format!(
                "{} does not offer a model listing",
                profile.kind
            )));
        }
        self.build(profile, TaskKind::Summary).await
    }
}
