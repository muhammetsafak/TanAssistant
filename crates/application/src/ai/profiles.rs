//! Use cases for provider profiles and the connection test.

use std::sync::Arc;

use tan_domain::{ProviderKind, ProviderProfile, ProviderProfileId, SecretId};

use super::{
    AiUseCaseError, AiUseCaseResult, MAX_BASE_URL_LEN, MAX_DISPLAY_NAME_LEN, MAX_MODEL_LEN,
};
use crate::credentials::{check_text, clean_line};
use crate::ports::{
    is_cli_provider, requires_api_key, Capabilities, ChatMessage, ChatModel, ChatRequest, Clock,
    IdGenerator, ModelCatalog, ProviderFactory, ProviderProfileRepository,
    TaskAssignmentRepository,
};

const DANGLING_SECRET: &str = "the referenced secret does not exist";

/// Lists provider profiles, ordered by display name.
pub struct ListProviderProfiles<P> {
    profiles: Arc<P>,
}

impl<P: ProviderProfileRepository> ListProviderProfiles<P> {
    /// Creates the use case.
    #[must_use]
    pub fn new(profiles: Arc<P>) -> Self {
        Self { profiles }
    }

    /// Runs it.
    pub async fn execute(&self) -> AiUseCaseResult<Vec<ProviderProfile>> {
        self.profiles
            .list()
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))
    }
}

/// Input of [`UpsertProviderProfile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertProviderProfileInput {
    /// Omit to create; set to update that profile.
    pub id: Option<ProviderProfileId>,
    /// Which provider.
    pub kind: ProviderKind,
    /// Name shown in the UI.
    pub display_name: String,
    /// Model identifier.
    pub model: String,
    /// Custom endpoint root; empty or `None` uses the provider default.
    pub base_url: Option<String>,
    /// The secret holding the API key.
    pub secret_ref: Option<SecretId>,
    /// Whether the model understands images. Only Ollama profiles use it; for the other
    /// providers the capability matrix decides and the stored flag follows it.
    pub supports_vision: bool,
}

/// Creates or updates a provider profile.
pub struct UpsertProviderProfile<P, A, I> {
    profiles: Arc<P>,
    assignments: Arc<A>,
    ids: Arc<I>,
}

impl<P, A, I> UpsertProviderProfile<P, A, I>
where
    P: ProviderProfileRepository,
    A: TaskAssignmentRepository,
    I: IdGenerator,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(profiles: Arc<P>, assignments: Arc<A>, ids: Arc<I>) -> Self {
        Self {
            profiles,
            assignments,
            ids,
        }
    }

    /// Validates and stores the profile.
    ///
    /// Besides field validation: a provider that needs an API key must reference a secret,
    /// a profile that has a key may only use an `https` base URL (plain `http` is allowed for
    /// localhost), and a change is refused when an existing task assignment would then point at
    /// a profile without that capability (a different provider kind, or an Ollama profile whose
    /// vision flag is switched off).
    pub async fn execute(
        &self,
        input: UpsertProviderProfileInput,
    ) -> AiUseCaseResult<ProviderProfile> {
        let display_name = clean_line("display name", &input.display_name, MAX_DISPLAY_NAME_LEN)?;
        let model = clean_line("model", &input.model, MAX_MODEL_LEN)?;
        if is_cli_provider(input.kind) {
            // A CLI provider runs on the sign-in of the installed binary (ADR-013): there is
            // no key to reference and no endpoint to point at.
            if input.secret_ref.is_some() {
                return Err(AiUseCaseError::Invalid(format!(
                    "{} does not take an API key",
                    input.kind
                )));
            }
            if input
                .base_url
                .as_deref()
                .is_some_and(|u| !u.trim().is_empty())
            {
                return Err(AiUseCaseError::Invalid(format!(
                    "{} does not take a base URL",
                    input.kind
                )));
            }
        }
        // Whenever a key will be sent, the endpoint must be encrypted, whatever the provider.
        let sends_key = requires_api_key(input.kind) || input.secret_ref.is_some();
        let base_url = clean_base_url(sends_key, input.base_url.as_deref())?;
        if requires_api_key(input.kind) && input.secret_ref.is_none() {
            return Err(AiUseCaseError::Invalid(format!(
                "{} needs an API key secret",
                input.kind
            )));
        }

        let is_update = input.id.is_some();
        let id = match input.id {
            Some(id) => {
                self.profiles
                    .get(&id)
                    .await
                    .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))?
                    .ok_or(AiUseCaseError::NotFound {
                        entity: "provider profile",
                    })?;
                id
            }
            None => ProviderProfileId::new(self.ids.new_id()),
        };

        let profile = ProviderProfile {
            id,
            kind: input.kind,
            display_name,
            model,
            base_url,
            secret_ref: input.secret_ref,
            // Only Ollama's vision support is the user's call; keep the flag consistent with
            // the matrix everywhere else.
            supports_vision: if input.kind == ProviderKind::Ollama {
                input.supports_vision
            } else {
                Capabilities::for_provider(input.kind).vision
            },
        };
        if is_update {
            self.check_assignments_still_valid(&profile).await?;
        }
        self.profiles
            .upsert(&profile)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))?;
        Ok(profile)
    }

    async fn check_assignments_still_valid(
        &self,
        profile: &ProviderProfile,
    ) -> AiUseCaseResult<()> {
        let caps = Capabilities::for_profile(profile);
        let assignments = self
            .assignments
            .list()
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))?;
        match assignments
            .iter()
            .find(|a| a.profile_id == profile.id && !caps.supports(a.task))
        {
            Some(broken) => Err(AiUseCaseError::CapabilityMissing {
                task: broken.task,
                provider: profile.kind,
            }),
            None => Ok(()),
        }
    }
}

/// Deletes a profile; the repository also removes the task assignments pointing at it.
pub struct DeleteProviderProfile<P> {
    profiles: Arc<P>,
}

impl<P: ProviderProfileRepository> DeleteProviderProfile<P> {
    /// Creates the use case.
    #[must_use]
    pub fn new(profiles: Arc<P>) -> Self {
        Self { profiles }
    }

    /// Runs it.
    pub async fn execute(&self, id: &ProviderProfileId) -> AiUseCaseResult<()> {
        self.profiles
            .delete(id)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))
    }
}

/// Outcome of a successful connection test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTestReport {
    /// Round-trip time of the test call, in milliseconds.
    pub latency_ms: u64,
    /// For Ollama: the models the server has.
    pub available_models: Vec<String>,
    /// For Ollama: whether the profile's model is among them; `None` for other providers.
    pub model_found: Option<bool>,
    /// For chat providers: the start of the model's reply.
    pub reply_preview: Option<String>,
}

const TEST_PROMPT: &str = "Reply with the single word: pong";
const TEST_MAX_TOKENS: u32 = 256;
const PREVIEW_CHARS: usize = 80;

/// Checks that a stored profile works: a short chat call, or for Ollama a model listing (a
/// local server may be up without the model being pulled).
pub struct TestProvider<P, F, C> {
    profiles: Arc<P>,
    factory: Arc<F>,
    clock: Arc<C>,
}

impl<P, F, C> TestProvider<P, F, C>
where
    P: ProviderProfileRepository,
    F: ProviderFactory,
    C: Clock,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(profiles: Arc<P>, factory: Arc<F>, clock: Arc<C>) -> Self {
        Self {
            profiles,
            factory,
            clock,
        }
    }

    /// Runs the test against the stored profile.
    pub async fn execute(&self, id: &ProviderProfileId) -> AiUseCaseResult<ProviderTestReport> {
        let profile = self
            .profiles
            .get(id)
            .await
            .map_err(|e| AiUseCaseError::from_repository(e, DANGLING_SECRET))?
            .ok_or(AiUseCaseError::NotFound {
                entity: "provider profile",
            })?;
        let started = self.clock.now().get();

        let mut report = ProviderTestReport {
            latency_ms: 0,
            available_models: Vec::new(),
            model_found: None,
            reply_preview: None,
        };
        if profile.kind == ProviderKind::Ollama {
            let models = self.factory.catalog(&profile).await?.list_models().await?;
            report.model_found = Some(models.iter().any(|m| model_matches(m, &profile.model)));
            report.available_models = models;
        } else {
            let reply = self
                .factory
                .chat(&profile)
                .await?
                .complete(ChatRequest {
                    system: None,
                    messages: vec![ChatMessage::user(TEST_PROMPT)],
                    temperature: None,
                    max_tokens: Some(TEST_MAX_TOKENS),
                    json_schema: None,
                })
                .await?;
            report.reply_preview = Some(reply.text.chars().take(PREVIEW_CHARS).collect());
        }
        let finished = self.clock.now().get();
        report.latency_ms = u64::try_from(finished.saturating_sub(started)).unwrap_or(0);
        Ok(report)
    }
}

/// Ollama lists models as `name:tag`; a profile may omit the `:latest` tag.
fn model_matches(listed: &str, wanted: &str) -> bool {
    listed == wanted || listed.strip_suffix(":latest") == Some(wanted)
}

/// Validates an optional base URL and returns it normalised (no trailing slash).
/// `sends_key`: an API key will be sent to this endpoint, so it must not be plain `http`
/// (except to localhost).
fn clean_base_url(sends_key: bool, raw: Option<&str>) -> AiUseCaseResult<Option<String>> {
    let Some(url) = raw.map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    check_text("base URL", url, MAX_BASE_URL_LEN)?;
    let invalid = |why: &str| Err(AiUseCaseError::Invalid(format!("base URL {why}")));
    if url.chars().any(char::is_whitespace) {
        return invalid("must not contain whitespace");
    }
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return invalid("must start with http:// or https://");
    };
    if url.contains('?') || url.contains('#') {
        return invalid("must not contain a query or fragment");
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return invalid("has no host");
    }
    if authority.contains('@') {
        return invalid("must not contain credentials");
    }
    if !secure && sends_key && !is_loopback(authority) {
        return invalid("must use https, because the API key is sent with every request (http is allowed for localhost only)");
    }
    Ok(Some(url.trim_end_matches('/').to_owned()))
}

/// Whether the `host[:port]` authority is a loopback address.
fn is_loopback(authority: &str) -> bool {
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::{clean_base_url, is_loopback, model_matches};

    #[test]
    fn base_url_is_normalised_and_checked() {
        let ok = |kind, url| clean_base_url(kind, Some(url)).unwrap();
        assert_eq!(
            ok(true, " https://proxy.example/v1/ "),
            Some("https://proxy.example/v1".to_owned())
        );
        assert_eq!(ok(true, "  "), None);
        assert!(clean_base_url(false, Some("http://10.0.0.5:11434")).is_ok());
        assert!(clean_base_url(true, Some("http://localhost:8080/v1")).is_ok());
        for bad in [
            "ftp://x",
            "http://proxy.example/v1",
            "https://user:pw@proxy.example",
            "https://proxy.example/v1?x=1",
            "https://",
            "https://a b",
        ] {
            assert!(clean_base_url(true, Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("localhost:11434"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("[::1]:8080"));
        assert!(!is_loopback("localhost.evil.example"));
    }

    #[test]
    fn ollama_latest_tag_is_optional() {
        assert!(model_matches("llama3.2:latest", "llama3.2"));
        assert!(model_matches("llama3.2:3b", "llama3.2:3b"));
        assert!(!model_matches("llama3.2:3b", "llama3.2"));
    }
}
