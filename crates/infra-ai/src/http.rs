//! Shared HTTP plumbing for every adapter: timeouts, retry with exponential backoff, error
//! classification and API-key redaction.
//!
//! Retry policy: only HTTP 429, HTTP 5xx and connection-level failures are retried. A timeout is
//! not retried (it already cost the full timeout) and neither are 4xx answers. A 429 that asks
//! for a wait longer than [`HttpSettings::max_retry_after`] is returned to the caller at once.
//!
//! Redaction: error messages that come from a provider are scrubbed of the API key (an exact
//! match) and of key-looking tokens before they reach an [`AiError`] or a log line; request
//! headers and bodies are never logged.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tan_application::ports::{AiError, AiResult, JsonSchema, SecretValue};

/// Text that replaces a redacted secret.
pub const REDACTED: &str = "[REDACTED]";

/// Longest provider message kept in an error, in characters.
const MAX_MESSAGE_CHARS: usize = 300;

/// Timeouts and retry behaviour shared by all providers.
#[derive(Debug, Clone)]
pub struct HttpSettings {
    /// Total time allowed for one request, including reading the body.
    pub timeout: Duration,
    /// Time allowed for establishing the connection.
    pub connect_timeout: Duration,
    /// Retries after the first attempt (so up to `max_retries + 1` requests).
    pub max_retries: u32,
    /// Delay before the first retry; doubles on every further retry.
    pub backoff_base: Duration,
    /// Upper bound of the backoff delay.
    pub backoff_max: Duration,
    /// Longest `Retry-After` the client is willing to sleep through.
    pub max_retry_after: Duration,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(180),
            connect_timeout: Duration::from_secs(15),
            max_retries: 2,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(8),
            max_retry_after: Duration::from_secs(30),
        }
    }
}

/// A `reqwest` client with the retry/redaction policy applied. Cheap to clone.
#[derive(Debug, Clone)]
pub struct HttpClient {
    client: Client,
    settings: HttpSettings,
}

impl HttpClient {
    /// Builds the client.
    pub fn new(settings: HttpSettings) -> AiResult<Self> {
        let client = Client::builder()
            .timeout(settings.timeout)
            .connect_timeout(settings.connect_timeout)
            .user_agent(concat!("tan/", env!("CARGO_PKG_VERSION")))
            // Provider APIs do not redirect; refusing keeps `x-api-key` / `x-goog-api-key`
            // from being forwarded to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AiError::Network("cannot initialise the HTTP client".to_owned()))?;
        Ok(Self { client, settings })
    }

    /// Sends the request built by `build` (called again for every retry) and parses the JSON
    /// answer. `secret` is the API key to scrub from messages.
    pub async fn send_json<T, F>(
        &self,
        provider: &'static str,
        secret: Option<&str>,
        build: F,
    ) -> AiResult<T>
    where
        T: DeserializeOwned,
        F: Fn(&Client) -> RequestBuilder + Send + Sync,
    {
        let body = self.send_text(provider, secret, build).await?;
        serde_json::from_str(&body).map_err(|_| AiError::Provider {
            status: 200,
            message: "the response could not be understood".to_owned(),
        })
    }

    /// Sends the request and returns the raw success body.
    pub async fn send_text<F>(
        &self,
        provider: &'static str,
        secret: Option<&str>,
        build: F,
    ) -> AiResult<String>
    where
        F: Fn(&Client) -> RequestBuilder + Send + Sync,
    {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            tracing::debug!(provider, attempt, "sending AI request");
            let error = match self.attempt(secret, build(&self.client)).await {
                Ok(body) => return Ok(body),
                Err(error) => error,
            };
            let Some(delay) = self.retry_delay(attempt, &error) else {
                tracing::warn!(provider, attempt, error = %error, "AI request failed");
                return Err(error);
            };
            tracing::warn!(
                provider,
                attempt,
                delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                error = %error,
                "AI request failed, retrying"
            );
            tokio::time::sleep(delay).await;
        }
    }

    async fn attempt(&self, secret: Option<&str>, request: RequestBuilder) -> AiResult<String> {
        let response = request
            .send()
            .await
            .map_err(|e| transport_error(&e, secret))?;
        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let body = response
            .text()
            .await
            .map_err(|e| transport_error(&e, secret))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(classify(status.as_u16(), retry_after, &body, secret))
        }
    }

    /// How long to wait before the next attempt, or `None` when the error is final.
    fn retry_delay(&self, attempt: u32, error: &AiError) -> Option<Duration> {
        if attempt > self.settings.max_retries {
            return None;
        }
        match error {
            AiError::RateLimited {
                retry_after: Some(wait),
            } => (*wait <= self.settings.max_retry_after).then_some(*wait),
            AiError::RateLimited { retry_after: None } | AiError::Network(_) => {
                Some(self.backoff(attempt))
            }
            AiError::Provider { status, .. } if *status >= 500 => Some(self.backoff(attempt)),
            _ => None,
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.settings
            .backoff_base
            .saturating_mul(factor)
            .min(self.settings.backoff_max)
    }
}

/// Where and as whom an adapter talks to its provider.
#[derive(Debug)]
pub struct ClientConfig {
    /// API root as documented by the provider (includes the version path where there is one).
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// API key; redacted in `Debug`.
    pub api_key: Option<SecretValue>,
}

impl ClientConfig {
    /// Creates a config; a trailing `/` of the base URL is dropped.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<SecretValue>,
    ) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            base_url,
            model: model.into(),
            api_key,
        }
    }

    /// `base_url` + `path` (the path starts with `/`).
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The raw key, for headers and redaction only.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.api_key.as_ref().map(SecretValue::expose)
    }
}

fn transport_error(error: &reqwest::Error, secret: Option<&str>) -> AiError {
    if error.is_timeout() {
        return AiError::Timeout;
    }
    // Connect and body failures get a fixed text; anything else is shown redacted.
    let text = error.to_string();
    let reason = if error.is_connect() {
        "could not connect to the provider".to_owned()
    } else if error.is_body() || error.is_decode() {
        "the connection broke while reading the answer".to_owned()
    } else {
        redact(&text, secret)
    };
    AiError::Network(reason)
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: f64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    // `try_from_secs_f64` rejects negative, NaN and overflowing values (`1e30`) instead of
    // panicking like `from_secs_f64`.
    Duration::try_from_secs_f64(seconds).ok()
}

/// Maps an HTTP error answer onto the typed error.
fn classify(
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
    secret: Option<&str>,
) -> AiError {
    let message = error_message(body, secret);
    match status {
        401 | 403 => AiError::Auth,
        429 => AiError::RateLimited { retry_after },
        408 => AiError::Timeout,
        400 | 413 | 422 => AiError::InvalidRequest(message),
        _ => AiError::Provider { status, message },
    }
}

/// Pulls the human-readable message out of the usual error envelopes, scrubbed and truncated.
fn error_message(body: &str, secret: Option<&str>) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let text = parsed
        .as_ref()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error"))
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(body)
        .trim();
    let text = redact(text, secret);
    if text.is_empty() {
        return "no details given".to_owned();
    }
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// Replaces the API key and key-looking tokens (`sk-...`, `AIza...`, even partially echoed
/// ones) with [`REDACTED`].
#[must_use]
pub fn redact(text: &str, secret: Option<&str>) -> String {
    let mut out = text.to_owned();
    if let Some(secret) = secret.filter(|s| s.len() >= 4) {
        out = out.replace(secret, REDACTED);
    }
    redact_key_like_tokens(&out)
}

fn redact_key_like_tokens(text: &str) -> String {
    const PREFIXES: [&str; 3] = ["sk-", "AIza", "key-"];
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '*' | '.');
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.len() >= 8 && PREFIXES.iter().any(|p| token.starts_with(p)) {
            out.push_str(REDACTED);
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in text.chars() {
        if is_token_char(c) {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// Base64 (standard alphabet) of `bytes`.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// A temperature as JSON, rounded so `0.2_f32` does not serialise as `0.20000000298...`.
#[must_use]
pub fn temperature_json(value: f32) -> Value {
    Value::from((f64::from(value) * 1000.0).round() / 1000.0)
}

/// Parses and checks a structured-output schema.
pub fn parse_schema(schema: &JsonSchema) -> AiResult<Value> {
    let name_ok = !schema.name.is_empty()
        && schema.name.len() <= 64
        && schema
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !name_ok {
        return Err(AiError::InvalidRequest(
            "the JSON schema name must be 1-64 characters of letters, digits, '_' or '-'"
                .to_owned(),
        ));
    }
    match serde_json::from_str::<Value>(&schema.schema_json) {
        Ok(value) if value.is_object() => Ok(value),
        _ => Err(AiError::InvalidRequest(
            "the JSON schema is not a valid JSON object".to_owned(),
        )),
    }
}

/// Audio MIME type from a file name's extension, for the formats the providers accept.
#[must_use]
pub fn audio_mime_from_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        "aac" => "audio/aac",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        audio_mime_from_name, classify, error_message, parse_retry_after, redact, REDACTED,
    };
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use std::time::Duration;
    use tan_application::ports::AiError;

    #[test]
    fn redacts_the_exact_key_and_key_like_tokens() {
        let text =
            "bad key sk-live-abcdefghij1234 / partial sk-proj-****abcd / AIzaSyD-example-key";
        let out = redact(text, Some("sk-live-abcdefghij1234"));
        assert!(!out.contains("abcdefghij1234"));
        assert!(!out.contains("sk-proj"));
        assert!(!out.contains("AIzaSyD"));
        assert!(out.contains(REDACTED));
        assert!(out.contains("bad key"));
    }

    #[test]
    fn leaves_ordinary_words_alone() {
        assert_eq!(
            redact("the task-queue is full", None),
            "the task-queue is full"
        );
    }

    #[test]
    fn classifies_statuses() {
        assert_eq!(classify(401, None, "{}", None), AiError::Auth);
        assert_eq!(classify(403, None, "{}", None), AiError::Auth);
        assert_eq!(
            classify(429, Some(Duration::from_secs(3)), "{}", None),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3))
            }
        );
        assert!(matches!(
            classify(400, None, "{}", None),
            AiError::InvalidRequest(_)
        ));
        assert!(matches!(
            classify(503, None, "{}", None),
            AiError::Provider { status: 503, .. }
        ));
    }

    #[test]
    fn extracts_messages_from_known_envelopes() {
        assert_eq!(
            error_message(r#"{"error":{"message":"nope"}}"#, None),
            "nope"
        );
        assert_eq!(
            error_message(r#"{"error":"model missing"}"#, None),
            "model missing"
        );
        assert_eq!(error_message("plain text", None), "plain text");
        assert_eq!(error_message("", None), "no details given");
        assert_eq!(error_message(&"x".repeat(1000), None).chars().count(), 300);
    }

    #[test]
    fn retry_after_is_read_in_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(12)));
        headers.insert(RETRY_AFTER, HeaderValue::from_static("soon"));
        assert_eq!(parse_retry_after(&headers), None);
        for hostile in ["1e30", "-5", "NaN", "inf"] {
            headers.insert(RETRY_AFTER, HeaderValue::from_str(hostile).unwrap());
            assert_eq!(parse_retry_after(&headers), None, "{hostile}");
        }
    }

    #[test]
    fn audio_mime_comes_from_the_extension() {
        assert_eq!(audio_mime_from_name("mic.WAV"), Some("audio/wav"));
        assert_eq!(audio_mime_from_name("a.m4a"), Some("audio/mp4"));
        assert_eq!(audio_mime_from_name("notes.txt"), None);
        assert_eq!(audio_mime_from_name("noext"), None);
    }
}
