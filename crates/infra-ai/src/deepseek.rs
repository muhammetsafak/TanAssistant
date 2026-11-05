//! `DeepSeek`: `OpenAI`-compatible chat (`POST {base}/chat/completions`, Bearer auth). No vision
//! and no transcription.
//!
//! `DeepSeek` has only a generic JSON mode (`response_format: {type: json_object}`), no schema
//! enforcement, and JSON mode requires the word "json" in the prompt. So for structured output
//! the schema is appended to the system prompt as an instruction.

use tan_application::ports::{AiResult, ChatModel, ChatRequest, ChatResponse};

use crate::http::{parse_schema, temperature_json, ClientConfig, HttpClient};
use crate::openai::{message_json, ChatCompletion};

const PROVIDER: &str = "deepseek";

/// Adapter for the `DeepSeek` API.
#[derive(Debug)]
pub struct DeepSeekClient {
    http: HttpClient,
    config: ClientConfig,
}

impl DeepSeekClient {
    /// Creates the adapter.
    #[must_use]
    pub fn new(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }
}

impl ChatModel for DeepSeekClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let mut system = request.system.clone();
        if let Some(schema) = &request.json_schema {
            let instruction = format!(
                "Respond only with a single valid JSON object that conforms to this JSON \
                 Schema:\n{}",
                parse_schema(schema)?
            );
            system = Some(match system {
                Some(existing) => format!("{existing}\n\n{instruction}"),
                None => instruction,
            });
        }

        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        if let Some(system) = &system {
            messages.push(serde_json::json!({ "role": "system", "content": system }));
        }
        messages.extend(request.messages.iter().map(message_json));

        let mut body = serde_json::json!({ "model": self.config.model, "messages": messages });
        if let Some(temperature) = request.temperature {
            body["temperature"] = temperature_json(temperature);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if request.json_schema.is_some() {
            body["response_format"] = serde_json::json!({ "type": "json_object" });
        }

        let url = self.config.url("/chat/completions");
        let bearer = format!("Bearer {}", self.config.key().unwrap_or_default());
        let completion: ChatCompletion = self
            .http
            .send_json(PROVIDER, self.config.key(), |client| {
                client
                    .post(&url)
                    .header("Authorization", &bearer)
                    .json(&body)
            })
            .await?;
        completion.into_response()
    }
}
