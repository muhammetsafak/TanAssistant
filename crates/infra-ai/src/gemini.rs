//! Gemini: `POST {base}/models/{model}:generateContent`, authenticated with the
//! `x-goog-api-key` header (never a `?key=` query parameter, so the key cannot end up in URLs).
//!
//! Vision sends images as `inlineData`. Transcription sends the audio as `inlineData` too and
//! asks the model for time-stamped segments through structured output
//! (`generationConfig.responseMimeType` + `responseJsonSchema`); the timestamps are what the
//! model reports (`MM:SS`) and are therefore approximate. Inline requests are limited to 20 MB in
//! total, so audio above [`MAX_INLINE_AUDIO_BYTES`] (base64 grows it by a third) is refused.

use serde::Deserialize;
use serde_json::{json, Value};
use tan_application::ports::{
    AiError, AiResult, AudioSource, ChatMessage, ChatModel, ChatRequest, ChatResponse, ChatRole,
    JsonSchema, TokenUsage, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
    VisionModel, VisionRequest,
};

use crate::http::{
    audio_mime_from_name, base64_encode, parse_schema, temperature_json, ClientConfig, HttpClient,
};

const PROVIDER: &str = "gemini";
/// 20 MB inline limit divided by the 4/3 base64 overhead, minus headroom for the prompt.
pub const MAX_INLINE_AUDIO_BYTES: usize = 14 * 1024 * 1024;
const IMAGE_TYPES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/heic",
    "image/heif",
];

/// Adapter for the Gemini API.
#[derive(Debug)]
pub struct GeminiClient {
    http: HttpClient,
    config: ClientConfig,
}

impl GeminiClient {
    /// Creates the adapter.
    #[must_use]
    pub fn new(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }

    /// `models/gemini-x` and `gemini-x` both name the same model; anything that could change
    /// the URL path is refused.
    fn model_path(&self) -> AiResult<String> {
        let name = self
            .config
            .model
            .strip_prefix("models/")
            .unwrap_or(&self.config.model);
        let ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if ok {
            Ok(format!("/models/{name}:generateContent"))
        } else {
            Err(AiError::InvalidRequest(
                "the model name contains unsupported characters".to_owned(),
            ))
        }
    }

    async fn generate(&self, body: Value) -> AiResult<ChatResponse> {
        let url = self.config.url(&self.model_path()?);
        let key = self.config.key().unwrap_or_default();
        let result = self
            .http
            .send_json::<Generation, _>(PROVIDER, self.config.key(), |client| {
                client.post(&url).header("x-goog-api-key", key).json(&body)
            })
            .await;
        match result {
            Ok(generation) => generation.into_response(),
            // Gemini answers an invalid key with HTTP 400 INVALID_ARGUMENT, not 401.
            Err(AiError::InvalidRequest(message))
                if message.to_ascii_lowercase().contains("api key") =>
            {
                Err(AiError::Auth)
            }
            Err(other) => Err(other),
        }
    }
}

fn request_body(
    system: Option<&str>,
    contents: &[Value],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    schema: Option<&JsonSchema>,
) -> AiResult<Value> {
    let mut body = json!({ "contents": contents });
    if let Some(system) = system {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    let mut config = serde_json::Map::new();
    if let Some(temperature) = temperature {
        config.insert("temperature".to_owned(), temperature_json(temperature));
    }
    if let Some(max_tokens) = max_tokens {
        config.insert("maxOutputTokens".to_owned(), json!(max_tokens));
    }
    if let Some(schema) = schema {
        config.insert("responseMimeType".to_owned(), json!("application/json"));
        config.insert("responseJsonSchema".to_owned(), parse_schema(schema)?);
    }
    if !config.is_empty() {
        body["generationConfig"] = Value::Object(config);
    }
    Ok(body)
}

fn content_json(message: &ChatMessage) -> Value {
    let role = match message.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "model",
    };
    json!({ "role": role, "parts": [{ "text": message.content }] })
}

impl ChatModel for GeminiClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let body = request_body(
            request.system.as_deref(),
            &request
                .messages
                .iter()
                .map(content_json)
                .collect::<Vec<_>>(),
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.generate(body).await
    }
}

impl VisionModel for GeminiClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        if request.images.is_empty() {
            return Err(AiError::InvalidRequest("no image was given".to_owned()));
        }
        let mut parts = Vec::with_capacity(request.images.len() + 1);
        for image in &request.images {
            if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
                return Err(AiError::InvalidRequest(format!(
                    "unsupported image type {}",
                    image.mime_type
                )));
            }
            parts.push(json!({
                "inlineData": {
                    "mimeType": image.mime_type,
                    "data": base64_encode(&image.data),
                },
            }));
        }
        parts.push(json!({ "text": request.prompt }));
        let body = request_body(
            request.system.as_deref(),
            &[json!({ "role": "user", "parts": parts })],
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.generate(body).await
    }
}

fn transcript_schema() -> JsonSchema {
    JsonSchema {
        name: "transcript".to_owned(),
        schema_json: json!({
            "type": "object",
            "properties": {
                "segments": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "start": { "type": "string", "description": "segment start, MM:SS" },
                            "end": { "type": "string", "description": "segment end, MM:SS" },
                            "text": { "type": "string", "description": "verbatim speech" },
                        },
                        "required": ["start", "end", "text"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["segments"],
            "additionalProperties": false,
        })
        .to_string(),
    }
}

fn transcription_prompt(request: &TranscriptionRequest) -> String {
    let language = request.language.as_ref().map_or_else(
        || "in the language(s) spoken in the audio".to_owned(),
        |l| format!("in the language with code \"{l}\""),
    );
    let mut prompt = format!(
        "Transcribe this audio verbatim {language}. Do not translate, summarise or add \
         commentary. Split the speech into consecutive segments of one or a few sentences and \
         give each segment's start and end time from the beginning of the audio in MM:SS \
         format. Skip silence and noise."
    );
    if let Some(hint) = &request.prompt {
        prompt.push_str("\nContext and vocabulary hints: ");
        prompt.push_str(hint);
    }
    prompt
}

impl TranscriptionModel for GeminiClient {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        let prompt = transcription_prompt(&request);
        let (mime_type, data) = match request.audio {
            AudioSource::Bytes {
                mime_type, data, ..
            } => (mime_type, data),
            AudioSource::Path(path) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let mime = audio_mime_from_name(name).ok_or_else(|| {
                    AiError::InvalidRequest("unrecognised audio file extension".to_owned())
                })?;
                let data = tokio::fs::read(&path).await.map_err(|_| {
                    AiError::InvalidRequest("the audio file cannot be read".to_owned())
                })?;
                (mime.to_owned(), data)
            }
        };
        if data.len() > MAX_INLINE_AUDIO_BYTES {
            return Err(AiError::InvalidRequest(
                "the audio is too large to send inline (about 14 MB); split it into shorter chunks"
                    .to_owned(),
            ));
        }
        let body = request_body(
            None,
            &[json!({
                "role": "user",
                "parts": [
                    { "inlineData": { "mimeType": mime_type, "data": base64_encode(&data) } },
                    { "text": prompt },
                ],
            })],
            // No temperature: Gemini 3 models are meant to run at their default of 1.0 and
            // degrade (loop) below it.
            None,
            None,
            Some(&transcript_schema()),
        )?;
        let reply = self.generate(body).await?;
        if reply.truncated {
            return Err(AiError::Provider {
                status: 200,
                message: "the transcript was cut off; use shorter audio chunks".to_owned(),
            });
        }
        parse_segments(&reply.text, request.language.as_ref())
    }
}

#[derive(Debug, Deserialize)]
struct TranscriptReply {
    #[serde(default)]
    segments: Vec<ReplySegment>,
}

#[derive(Debug, Deserialize)]
struct ReplySegment {
    start: String,
    end: String,
    #[serde(default)]
    text: String,
}

fn parse_segments(
    text: &str,
    language: Option<&tan_domain::LanguageCode>,
) -> AiResult<Vec<TranscriptSegmentDto>> {
    let reply: TranscriptReply = serde_json::from_str(text).map_err(|_| AiError::Provider {
        status: 200,
        message: "the transcript could not be understood".to_owned(),
    })?;
    Ok(reply
        .segments
        .into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .filter_map(|s| {
            let start_ms = parse_clock(&s.start)?;
            let end_ms = parse_clock(&s.end).unwrap_or(start_ms).max(start_ms);
            Some(TranscriptSegmentDto {
                start_ms,
                end_ms,
                text: s.text.trim().to_owned(),
                language: language.cloned(),
            })
        })
        .collect())
}

/// Parses `MM:SS`, `H:MM:SS` (optionally with fractional seconds) or plain seconds into
/// milliseconds.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn parse_clock(text: &str) -> Option<u64> {
    let mut seconds = 0.0_f64;
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in &parts {
        let value: f64 = part.trim().parse().ok()?;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    Some((seconds * 1000.0).round() as u64)
}

#[derive(Debug, Deserialize)]
struct Generation {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(rename = "usageMetadata")]
    usage: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: Option<CandidateContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ResponsePart {
    text: Option<String>,
    #[serde(default)]
    thought: bool,
}

#[derive(Debug, Deserialize)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt: u64,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates: u64,
}

impl Generation {
    fn into_response(self) -> AiResult<ChatResponse> {
        let Some(candidate) = self.candidates.into_iter().next() else {
            let reason = self
                .prompt_feedback
                .and_then(|f| f.block_reason)
                .unwrap_or_else(|| "no candidates returned".to_owned());
            return Err(AiError::InvalidRequest(format!(
                "the prompt was blocked: {reason}"
            )));
        };
        let text: String = candidate
            .content
            .map(|c| c.parts)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| !p.thought)
            .filter_map(|p| p.text)
            .collect();
        Ok(ChatResponse {
            text,
            usage: self.usage.map(|u| TokenUsage {
                input_tokens: u.prompt,
                output_tokens: u.candidates,
            }),
            truncated: candidate.finish_reason.as_deref() == Some("MAX_TOKENS"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_clock;

    #[test]
    fn clock_formats() {
        assert_eq!(parse_clock("00:07"), Some(7_000));
        assert_eq!(parse_clock("12:34"), Some(754_000));
        assert_eq!(parse_clock("1:02:03"), Some(3_723_000));
        assert_eq!(parse_clock("00:01.5"), Some(1_500));
        assert_eq!(parse_clock("42"), Some(42_000));
        assert_eq!(parse_clock("soon"), None);
        assert_eq!(parse_clock("1:2:3:4"), None);
    }
}
