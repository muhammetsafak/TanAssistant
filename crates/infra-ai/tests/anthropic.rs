//! Anthropic adapter against a mock server.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{AiError, ChatModel, VisionModel};
use tan_infra_ai::anthropic::AnthropicClient;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::{b64, chat_request, config, http, schema, vision_request, KEY, PNG};

fn client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::new(http(), config(format!("{}/v1", server.uri()), "claude-x"))
}

fn message(text: &str, stop_reason: &str) -> serde_json::Value {
    json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "thinking", "thinking": "..." }, { "type": "text", "text": text }],
        "stop_reason": stop_reason,
        "usage": { "input_tokens": 20, "output_tokens": 5 },
    })
}

fn body_json(request: &Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap()
}

#[tokio::test]
async fn chat_sends_the_documented_headers_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2026-10-21"))
        .and(body_partial_json(json!({
            "model": "claude-x",
            "max_tokens": 100,
            "system": "be brief",
            "messages": [{ "role": "user", "content": "hello" }],
        })))
        // The `temperature` parameter is rejected by this API version, so it is never sent.
        .and(|r: &Request| body_json(r).get("temperature").is_none())
        .and(|r: &Request| r.headers.get("authorization").is_none())
        .respond_with(ResponseTemplate::new(200).set_body_json(message("hi", "end_turn")))
        .expect(1)
        .mount(&server)
        .await;

    let reply = client(&server).complete(chat_request()).await.unwrap();
    assert_eq!(reply.text, "hi", "only text blocks are returned");
    let usage = reply.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (20, 5));
    assert!(!reply.truncated);
}

#[tokio::test]
async fn max_tokens_defaults_when_the_request_names_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "max_tokens": 8192 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(message("ok", "max_tokens")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.max_tokens = None;
    let reply = client(&server).complete(request).await.unwrap();
    assert!(reply.truncated);
}

#[tokio::test]
async fn structured_output_goes_through_output_config() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "output_config": { "format": { "type": "json_schema", "schema": { "type": "object" } } },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(message("{}", "end_turn")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.json_schema = Some(schema());
    client(&server).complete(request).await.unwrap();
}

#[tokio::test]
async fn vision_sends_base64_image_blocks_before_the_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": b64(PNG) } },
                    { "type": "text", "text": "what is on the slide?" },
                ],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(message("a slide", "end_turn")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server)
            .describe(vision_request())
            .await
            .unwrap()
            .text,
        "a slide"
    );
}

#[tokio::test]
async fn a_refusal_is_an_error_not_an_empty_reply() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message("", "refusal")))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).complete(chat_request()).await,
        Err(AiError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn unauthorised_maps_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid x-api-key" },
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
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "99"))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(99))
        }
    );
}

#[tokio::test]
async fn overloaded_529_is_retried_then_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" },
        })))
        .expect(3)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::Provider {
            status: 529,
            message: "Overloaded".to_owned()
        }
    );
}
