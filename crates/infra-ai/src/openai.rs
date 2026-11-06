//! `OpenAI`: Chat Completions (`POST {base}/chat/completions`), vision through `image_url` data
//! URIs, and `POST {base}/audio/transcriptions`.
//!
//! Auth is `Authorization: Bearer <key>`. Structured output uses
//! `response_format: {type: json_schema, json_schema: {name, schema, strict: true}}` and the
//! length bound is `max_completion_tokens`. The chat helpers here are reused by the `DeepSeek`
//! adapter (it speaks the same wire format).

use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::{json, Value};
use tan_application::ports::{
    AiError, AiResult, AudioSource, ChatMessage, ChatModel, ChatRequest, ChatResponse, ChatRole,
    TokenUsage, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest, VisionModel,
    VisionRequest,
};
use tan_domain::LanguageCode;

use crate::http::{
    audio_mime_from_name, base64_encode, parse_schema, temperature_json, ClientConfig, HttpClient,
};

const PROVIDER: &str = "openai";
/// Upload limit of the transcription endpoint.
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Adapter for the `OpenAI` API (and `OpenAI`-compatible proxies via a custom base URL).
#[derive(Debug)]
pub struct OpenAiClient {
    http: HttpClient,
    config: ClientConfig,
}

impl OpenAiClient {
    /// Creates the adapter.
    #[must_use]
    pub fn new(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.config.key().unwrap_or_default())
    }

    async fn run_chat(&self, body: Value) -> AiResult<ChatResponse> {
        let url = self.config.url("/chat/completions");
        let completion: ChatCompletion = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                client
                    .post(&url)
                    .header("Authorization", self.bearer())
                    .json(&body)
            })
            .await?;
        completion.into_response()
    }
}

impl ChatModel for OpenAiClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let messages = request.messages.iter().map(message_json).collect();
        let body = chat_body(
            &self.config.model,
            request.system.as_deref(),
            messages,
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run_chat(body).await
    }
}

impl VisionModel for OpenAiClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        let user = vision_message(&request)?;
        let body = chat_body(
            &self.config.model,
            request.system.as_deref(),
            vec![user],
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run_chat(body).await
    }
}

impl TranscriptionModel for OpenAiClient {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        let (file_name, mime_type, data) = load_audio(request.audio).await?;
        if data.len() > MAX_AUDIO_BYTES {
            return Err(AiError::InvalidRequest(
                "the audio is larger than 25 MB; split it into shorter chunks".to_owned(),
            ));
        }
        // Checked against the API reference (2026-11-06): `verbose_json` and
        // `timestamp_granularities[]` exist for `whisper-1` only; `gpt-transcribe` and the
        // `gpt-4o-*-transcribe` models accept `json` (text, no timestamps), so they get a single
        // segment per request and the caller supplies the timing by sending short pieces.
        let with_segments = self.config.model.starts_with("whisper");
        // `gpt-transcribe` takes `languages[]` (and must not get `language` as well); whisper-1
        // and the older models take a single `language`.
        let new_style = self.config.model.starts_with("gpt-transcribe");
        let language = request
            .language
            .as_ref()
            .map(|l| language_hint(l, new_style));
        let url = self.config.url("/audio/transcriptions");
        let model = self.config.model.clone();
        let prompt = request.prompt.clone();

        let response: TranscriptionResponse = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                let file = Part::bytes(data.clone())
                    .file_name(file_name.clone())
                    .mime_str(&mime_type)
                    .unwrap_or_else(|_| Part::bytes(data.clone()).file_name(file_name.clone()));
                let mut form = Form::new().part("file", file).text("model", model.clone());
                if with_segments {
                    form = form
                        .text("response_format", "verbose_json")
                        .text("timestamp_granularities[]", "segment");
                } else {
                    form = form.text("response_format", "json");
                }
                if let Some(language) = &language {
                    let field = if new_style { "languages[]" } else { "language" };
                    form = form.text(field, language.clone());
                }
                if let Some(prompt) = &prompt {
                    form = form.text("prompt", prompt.clone());
                }
                client
                    .post(&url)
                    .header("Authorization", self.bearer())
                    .multipart(form)
            })
            .await?;
        Ok(response.into_segments(request.language, request.duration_ms))
    }
}

/// Reads the audio into memory and resolves its file name and MIME type.
async fn load_audio(source: AudioSource) -> AiResult<(String, String, Vec<u8>)> {
    match source {
        AudioSource::Bytes {
            file_name,
            mime_type,
            data,
        } => Ok((file_name, mime_type, data)),
        AudioSource::Path(path) => {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("audio")
                .to_owned();
            let mime_type = audio_mime_from_name(&file_name).ok_or_else(|| {
                AiError::InvalidRequest("unrecognised audio file extension".to_owned())
            })?;
            let data = tokio::fs::read(&path)
                .await
                .map_err(|_| AiError::InvalidRequest("the audio file cannot be read".to_owned()))?;
            Ok((file_name, mime_type.to_owned(), data))
        }
    }
}

/// `pt-br` -> `pt`: the transcription endpoint takes ISO 639-1 codes. `gpt-transcribe` also
/// accepts the regional Chinese locales (`zh-cn`, `zh-tw`, `zh-hk`), which are kept whole.
fn language_hint(language: &LanguageCode, regional_chinese: bool) -> String {
    let code = language.as_str();
    if regional_chinese && matches!(code, "zh-cn" | "zh-tw" | "zh-hk") {
        return code.to_owned();
    }
    code.split('-').next().unwrap_or_default().to_owned()
}

fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
}

/// A plain-text message in the chat-completions wire format.
pub(crate) fn message_json(message: &ChatMessage) -> Value {
    json!({ "role": role_name(message.role), "content": message.content })
}

/// The user message of a vision request: the prompt text followed by the images.
pub(crate) fn vision_message(request: &VisionRequest) -> AiResult<Value> {
    if request.images.is_empty() {
        return Err(AiError::InvalidRequest("no image was given".to_owned()));
    }
    let mut parts = vec![json!({ "type": "text", "text": request.prompt })];
    for image in &request.images {
        if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
            return Err(AiError::InvalidRequest(format!(
                "unsupported image type {}",
                image.mime_type
            )));
        }
        let url = format!(
            "data:{};base64,{}",
            image.mime_type,
            base64_encode(&image.data)
        );
        parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
    }
    Ok(json!({ "role": "user", "content": parts }))
}

/// The chat-completions request body.
fn chat_body(
    model: &str,
    system: Option<&str>,
    messages: Vec<Value>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    schema: Option<&tan_application::ports::JsonSchema>,
) -> AiResult<Value> {
    let mut all = Vec::with_capacity(messages.len() + 1);
    if let Some(system) = system {
        all.push(json!({ "role": "system", "content": system }));
    }
    all.extend(messages);
    let mut body = json!({ "model": model, "messages": all });
    if let Some(temperature) = temperature {
        body["temperature"] = temperature_json(temperature);
    }
    if let Some(max_tokens) = max_tokens {
        body["max_completion_tokens"] = json!(max_tokens);
    }
    if let Some(schema) = schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": schema.name,
                "schema": parse_schema(schema)?,
                "strict": true,
            },
        });
    }
    Ok(body)
}

/// Response of a chat-completions call (also `DeepSeek`'s).
#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletion {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

impl ChatCompletion {
    pub(crate) fn into_response(self) -> AiResult<ChatResponse> {
        let choice = self.choices.into_iter().next().ok_or(AiError::Provider {
            status: 200,
            message: "the response contains no choices".to_owned(),
        })?;
        Ok(ChatResponse {
            text: choice.message.content.unwrap_or_default(),
            usage: self.usage.map(|u| TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            }),
            truncated: choice.finish_reason.as_deref() == Some("length"),
        })
    }
}

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
    language: Option<String>,
    /// `gpt-transcribe` names the detected languages here instead of `language`.
    languages: Option<Vec<WireLanguage>>,
    duration: Option<f64>,
    segments: Option<Vec<WireSegment>>,
}

#[derive(Debug, Deserialize)]
struct WireLanguage {
    code: String,
}

#[derive(Debug, Deserialize)]
struct WireSegment {
    start: f64,
    end: f64,
    #[serde(default)]
    text: String,
}

/// Seconds (as the API reports them) to whole milliseconds.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

impl TranscriptionResponse {
    fn into_segments(
        self,
        requested: Option<LanguageCode>,
        duration_hint_ms: Option<u64>,
    ) -> Vec<TranscriptSegmentDto> {
        // verbose_json names the language in English ("turkish"); only a short code parses.
        let language = requested
            .or_else(|| {
                self.languages
                    .as_deref()
                    .and_then(<[WireLanguage]>::first)
                    .and_then(|l| LanguageCode::new(&l.code).ok())
            })
            .or_else(|| {
                self.language
                    .as_deref()
                    .and_then(|l| LanguageCode::new(l).ok())
            });
        let segments: Vec<TranscriptSegmentDto> = self
            .segments
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| {
                let start_ms = seconds_to_ms(s.start);
                TranscriptSegmentDto {
                    start_ms,
                    end_ms: seconds_to_ms(s.end).max(start_ms),
                    text: s.text.trim().to_owned(),
                    language: language.clone(),
                }
            })
            .collect();
        if !segments.is_empty() || self.text.trim().is_empty() {
            return segments;
        }
        // The model returned no timestamps: one segment spanning the whole audio.
        let end_ms = self
            .duration
            .map(seconds_to_ms)
            .or(duration_hint_ms)
            .unwrap_or(0);
        vec![TranscriptSegmentDto {
            start_ms: 0,
            end_ms,
            text: self.text.trim().to_owned(),
            language,
        }]
    }
}
