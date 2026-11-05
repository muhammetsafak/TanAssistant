//! Ollama adapter against a mock server.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{AiError, ChatModel, ModelCatalog, SecretValue, VisionModel};
use tan_infra_ai::ollama::OllamaClient;
use tan_infra_ai::ClientConfig;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::{b64, chat_request, http, schema, vision_request, KEY, PNG};

fn client(server: &MockServer, key: Option<&str>) -> OllamaClient {
    OllamaClient::new(
        http(),
        ClientConfig::new(server.uri(), "llama3.2", key.map(SecretValue::new)),
    )
}

fn reply(text: &str) -> serde_json::Value {
    json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": text },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 11,
        "eval_count": 18,
    })
}

#[tokio::test]
async fn chat_is_non_streaming_and_sends_no_auth_without_a_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_partial_json(json!({
            "model": "llama3.2",
            "stream": false,
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hello" },
            ],
            "options": { "temperature": 0.2, "num_predict": 100 },
        })))
        .and(|r: &Request| r.headers.get("authorization").is_none())
        .respond_with(ResponseTemplate::new(200).set_body_json(reply("selam")))
        .expect(1)
        .mount(&server)
        .await;
    let response = client(&server, None)
        .complete(chat_request())
        .await
        .unwrap();
    assert_eq!(response.text, "selam");
    let usage = response.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 18));
}

#[tokio::test]
async fn a_configured_key_is_sent_as_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply("ok")))
        .expect(1)
        .mount(&server)
        .await;
    client(&server, Some(KEY))
        .complete(chat_request())
        .await
        .unwrap();
}

#[tokio::test]
async fn structured_output_uses_the_format_field() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "format": { "type": "object" } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply("{}")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.json_schema = Some(schema());
    client(&server, None).complete(request).await.unwrap();
}

#[tokio::test]
async fn vision_sends_base64_images_on_the_user_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "messages": [{
                "role": "user",
                "content": "what is on the slide?",
                "images": [b64(PNG)],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply("a slide")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, None)
            .describe(vision_request())
            .await
            .unwrap()
            .text,
        "a slide"
    );
}

#[tokio::test]
async fn a_text_only_model_error_surfaces_with_its_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "this model does not support images" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, None)
        .describe(vision_request())
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::InvalidRequest(m) if m.contains("does not support images")));
}

#[tokio::test]
async fn a_missing_model_is_a_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "error": "model 'llama3.2' not found" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, None)
        .complete(chat_request())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        AiError::Provider {
            status: 404,
            message: "model 'llama3.2' not found".to_owned()
        }
    );
}

#[tokio::test]
async fn unauthorised_and_rate_limited_are_typed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "31"))
        .expect(1)
        .mount(&server)
        .await;
    let ollama = client(&server, Some(KEY));
    assert_eq!(
        ollama.complete(chat_request()).await.unwrap_err(),
        AiError::Auth
    );
    assert_eq!(
        ollama.complete(chat_request()).await.unwrap_err(),
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(31))
        }
    );
}

#[tokio::test]
async fn lists_the_models_the_server_has() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "name": "llama3.2:latest", "model": "llama3.2:latest", "size": 1 },
                { "name": "llava:7b", "model": "llava:7b", "size": 2 },
            ],
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, None).list_models().await.unwrap(),
        vec!["llama3.2:latest".to_owned(), "llava:7b".to_owned()]
    );
}

#[tokio::test]
async fn a_server_that_is_down_is_a_network_error() {
    let address = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let ollama = OllamaClient::new(
        http(),
        ClientConfig::new(format!("http://{address}"), "llama3.2", None),
    );
    assert!(matches!(
        ollama.list_models().await,
        Err(AiError::Network(_))
    ));
}
