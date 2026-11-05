//! `DeepSeek` adapter against a mock server.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{AiError, ChatModel};
use tan_infra_ai::deepseek::DeepSeekClient;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::{chat_request, config, http, schema, KEY};

fn client(server: &MockServer) -> DeepSeekClient {
    DeepSeekClient::new(http(), config(server.uri(), "deepseek-flash"))
}

fn completion(text: &str) -> serde_json::Value {
    json!({
        "choices": [{ "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 2 },
    })
}

#[tokio::test]
async fn chat_uses_bearer_auth_and_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(body_partial_json(json!({
            "model": "deepseek-flash",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hello" },
            ],
            "max_tokens": 100,
        })))
        .and(|r: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            body.get("response_format").is_none() && body.get("max_completion_tokens").is_none()
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("merhaba")))
        .expect(1)
        .mount(&server)
        .await;
    let reply = client(&server).complete(chat_request()).await.unwrap();
    assert_eq!(reply.text, "merhaba");
    assert_eq!(reply.usage.unwrap().output_tokens, 2);
}

#[tokio::test]
async fn structured_output_uses_json_mode_with_the_schema_in_the_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({ "response_format": { "type": "json_object" } }),
        ))
        .and(|r: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            body.pointer("/messages/0/content")
                .and_then(|c| c.as_str())
                .is_some_and(|c| {
                    c.starts_with("be brief") && c.contains("JSON Schema") && c.contains("summary")
                })
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("{\"summary\":\"x\"}")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.json_schema = Some(schema());
    assert_eq!(
        client(&server).complete(request).await.unwrap().text,
        "{\"summary\":\"x\"}"
    );
}

#[tokio::test]
async fn unauthorised_maps_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "Authentication Fails", "type": "authentication_error" },
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::Auth
    );
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "45"))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(45))
        }
    );
}
