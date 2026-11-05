//! Anthropic: Messages API (`POST {base}/messages`), text and image input.
//!
//! Headers: `x-api-key`, `anthropic-version` (see [`ANTHROPIC_VERSION`]) and a JSON content
//! type. Structured output is `output_config.format = {type: json_schema, schema}`. The
//! `temperature` parameter is not sent: API version `2026-10-21` rejects it with a 400. There is
//! no transcription capability.

use serde::Deserialize;
use serde_json::{json, Value};
use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, ChatRole, JsonSchema, TokenUsage,
    VisionModel, VisionRequest,
};

use crate::defaults::{ANTHROPIC_VERSION, DEFAULT_MAX_TOKENS};
use crate::http::{base64_encode, parse_schema, ClientConfig, HttpClient};

const PROVIDER: &str = "anthropic";
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Adapter for the Anthropic API.
#[derive(Debug)]
pub struct AnthropicClient {
    http: HttpClient,
    config: ClientConfig,
}

impl AnthropicClient {
    /// Creates the adapter.
    #[must_use]
    pub fn new(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }

    async fn run(&self, body: Value) -> AiResult<ChatResponse> {
        let url = self.config.url("/messages");
        let key = self.config.key().unwrap_or_default();
        let message: Message = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                client
                    .post(&url)
                    .header("x-api-key", key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .json(&body)
            })
            .await?;
        message.into_response()
    }

    fn body(
        &self,
        system: Option<&str>,
        messages: &[Value],
        max_tokens: Option<u32>,
        schema: Option<&JsonSchema>,
    ) -> AiResult<Value> {
        let mut body = json!({
            "model": self.config.model,
            "max_tokens": max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": messages,
        });
        if let Some(system) = system {
            body["system"] = json!(system);
        }
        if let Some(schema) = schema {
            body["output_config"] = json!({
                "format": { "type": "json_schema", "schema": parse_schema(schema)? },
            });
        }
        Ok(body)
    }
}

impl ChatModel for AnthropicClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                };
                json!({ "role": role, "content": m.content })
            })
            .collect();
        let body = self.body(
            request.system.as_deref(),
            &messages,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run(body).await
    }
}

impl VisionModel for AnthropicClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        if request.images.is_empty() {
            return Err(AiError::InvalidRequest("no image was given".to_owned()));
        }
        // Images first, the instruction after them: the order Anthropic recommends.
        let mut parts = Vec::with_capacity(request.images.len() + 1);
        for image in &request.images {
            if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
                return Err(AiError::InvalidRequest(format!(
                    "unsupported image type {}",
                    image.mime_type
                )));
            }
            parts.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": base64_encode(&image.data),
                },
            }));
        }
        parts.push(json!({ "type": "text", "text": request.prompt }));
        let body = self.body(
            request.system.as_deref(),
            &[json!({ "role": "user", "content": parts })],
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run(body).await
    }
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    content: Vec<Block>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl Message {
    fn into_response(self) -> AiResult<ChatResponse> {
        if self.stop_reason.as_deref() == Some("refusal") {
            return Err(AiError::InvalidRequest(
                "the model declined to answer".to_owned(),
            ));
        }
        let text: String = self
            .content
            .into_iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text)
            .collect();
        Ok(ChatResponse {
            text,
            usage: self.usage.map(|u| TokenUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            }),
            truncated: self.stop_reason.as_deref() == Some("max_tokens"),
        })
    }
}
