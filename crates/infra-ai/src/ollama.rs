//! Ollama: `POST {base}/api/chat` (non-streaming) and `GET {base}/api/tags` for the model list.
//!
//! Ollama needs no key; one is sent as a Bearer token only when the profile has one (hosted
//! or proxied servers). Images go in the message's `images` array (base64). Whether a model
//! understands images depends on the model: a text-only model answers with an error, which
//! surfaces as [`AiError::InvalidRequest`] or [`AiError::Provider`]. Structured output uses
//! `format` with the JSON schema. There is no transcription (local whisper does that).

use serde::Deserialize;
use serde_json::{json, Value};
use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, ChatRole, JsonSchema, ModelCatalog,
    TokenUsage, VisionModel, VisionRequest,
};

use crate::http::{base64_encode, parse_schema, temperature_json, ClientConfig, HttpClient};

const PROVIDER: &str = "ollama";

/// Adapter for a (local or remote) Ollama server.
#[derive(Debug)]
pub struct OllamaClient {
    http: HttpClient,
    config: ClientConfig,
}

impl OllamaClient {
    /// Creates the adapter.
    #[must_use]
    pub fn new(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }

    fn auth(&self) -> Option<String> {
        self.config.key().map(|key| format!("Bearer {key}"))
    }

    async fn run(&self, body: Value) -> AiResult<ChatResponse> {
        let url = self.config.url("/api/chat");
        let auth = self.auth();
        let reply: ChatReply = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                let request = client.post(&url).json(&body);
                match &auth {
                    Some(auth) => request.header("Authorization", auth),
                    None => request,
                }
            })
            .await?;
        Ok(reply.into_response())
    }

    fn body(
        &self,
        messages: &[Value],
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        schema: Option<&JsonSchema>,
    ) -> AiResult<Value> {
        let mut body = json!({
            "model": self.config.model,
            "messages": messages,
            "stream": false,
        });
        let mut options = serde_json::Map::new();
        if let Some(temperature) = temperature {
            options.insert("temperature".to_owned(), temperature_json(temperature));
        }
        if let Some(max_tokens) = max_tokens {
            options.insert("num_predict".to_owned(), json!(max_tokens));
        }
        if !options.is_empty() {
            body["options"] = Value::Object(options);
        }
        if let Some(schema) = schema {
            body["format"] = parse_schema(schema)?;
        }
        Ok(body)
    }
}

fn system_message(system: Option<&str>) -> Option<Value> {
    system.map(|text| json!({ "role": "system", "content": text }))
}

impl ChatModel for OllamaClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        messages.extend(system_message(request.system.as_deref()));
        for message in &request.messages {
            let role = match message.role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            messages.push(json!({ "role": role, "content": message.content }));
        }
        let body = self.body(
            &messages,
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run(body).await
    }
}

impl VisionModel for OllamaClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        if request.images.is_empty() {
            return Err(AiError::InvalidRequest("no image was given".to_owned()));
        }
        let images: Vec<String> = request
            .images
            .iter()
            .map(|image| base64_encode(&image.data))
            .collect();
        let mut messages = Vec::with_capacity(2);
        messages.extend(system_message(request.system.as_deref()));
        messages.push(json!({ "role": "user", "content": request.prompt, "images": images }));
        let body = self.body(
            &messages,
            request.temperature,
            request.max_tokens,
            request.json_schema.as_ref(),
        )?;
        self.run(body).await
    }
}

impl ModelCatalog for OllamaClient {
    async fn list_models(&self) -> AiResult<Vec<String>> {
        let url = self.config.url("/api/tags");
        let auth = self.auth();
        let tags: Tags = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                let request = client.get(&url);
                match &auth {
                    Some(auth) => request.header("Authorization", auth),
                    None => request,
                }
            })
            .await?;
        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }
}

#[derive(Debug, Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ChatReply {
    message: ReplyMessage,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ReplyMessage {
    #[serde(default)]
    content: String,
}

impl ChatReply {
    fn into_response(self) -> ChatResponse {
        let usage = match (self.prompt_eval_count, self.eval_count) {
            (None, None) => None,
            (input, output) => Some(TokenUsage {
                input_tokens: input.unwrap_or(0),
                output_tokens: output.unwrap_or(0),
            }),
        };
        ChatResponse {
            text: self.message.content,
            usage,
            truncated: self.done_reason.as_deref() == Some("length"),
        }
    }
}
