//! Helpers shared by the adapter tests (compiled as their own, empty test binary too).
#![allow(dead_code, clippy::must_use_candidate)]

use std::time::Duration;

use tan_application::ports::{ChatMessage, ChatRequest, ImageInput, SecretValue, VisionRequest};
use tan_infra_ai::{ClientConfig, HttpClient, HttpSettings};

/// A fake API key; tests assert it never shows up in errors or logs.
pub const KEY: &str = "sk-test-0123456789abcdef";

/// Minimal bytes that stand in for a PNG file.
pub const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];

/// Client with two retries and no waiting between them.
pub fn http() -> HttpClient {
    http_with(2, Duration::from_secs(5))
}

pub fn http_with(max_retries: u32, timeout: Duration) -> HttpClient {
    HttpClient::new(HttpSettings {
        timeout,
        connect_timeout: Duration::from_secs(5),
        max_retries,
        backoff_base: Duration::ZERO,
        backoff_max: Duration::ZERO,
        max_retry_after: Duration::from_secs(30),
    })
    .expect("http client")
}

pub fn config(base_url: impl Into<String>, model: &str) -> ClientConfig {
    ClientConfig::new(base_url, model, Some(SecretValue::new(KEY)))
}

pub fn chat_request() -> ChatRequest {
    ChatRequest {
        system: Some("be brief".to_owned()),
        messages: vec![ChatMessage::user("hello")],
        temperature: Some(0.2),
        max_tokens: Some(100),
        json_schema: None,
    }
}

pub fn vision_request() -> VisionRequest {
    VisionRequest {
        system: None,
        prompt: "what is on the slide?".to_owned(),
        images: vec![ImageInput {
            mime_type: "image/png".to_owned(),
            data: PNG.to_vec(),
        }],
        temperature: None,
        max_tokens: Some(50),
        json_schema: None,
    }
}

pub fn schema() -> tan_application::ports::JsonSchema {
    tan_application::ports::JsonSchema {
        name: "notes".to_owned(),
        schema_json: r#"{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"],"additionalProperties":false}"#
            .to_owned(),
    }
}

pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
