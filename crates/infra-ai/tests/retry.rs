//! Shared retry/timeout policy, exercised through the `OpenAI` adapter.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{AiError, ChatModel};
use tan_infra_ai::openai::OpenAiClient;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{chat_request, config, http, http_with};

fn client(server: &MockServer) -> OpenAiClient {
    OpenAiClient::new(http(), config(server.uri(), "m"))
}

fn ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{ "message": { "content": "fine" }, "finish_reason": "stop" }],
    }))
}

#[tokio::test]
async fn server_errors_are_retried_up_to_the_limit() {
    let server = MockServer::start().await;
    // max_retries = 2 -> exactly three requests, then the last error is returned.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({ "error": { "message": "boom" } })),
        )
        .expect(3)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::Provider {
            status: 500,
            message: "boom".to_owned()
        }
    );
}

#[tokio::test]
async fn retries_can_be_switched_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let no_retry = OpenAiClient::new(
        http_with(0, Duration::from_secs(5)),
        config(server.uri(), "m"),
    );
    assert!(matches!(
        no_retry.complete(chat_request()).await,
        Err(AiError::Provider { status: 503, .. })
    ));
}

#[tokio::test]
async fn a_transient_failure_followed_by_success_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap().text,
        "fine"
    );
}

#[tokio::test]
async fn rate_limits_are_retried_when_the_wait_is_short() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap().text,
        "fine"
    );
}

#[tokio::test]
async fn a_persistent_rate_limit_without_retry_after_stops_at_the_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(3)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).complete(chat_request()).await.unwrap_err(),
        AiError::RateLimited { retry_after: None }
    );
}

#[tokio::test]
async fn client_errors_are_never_retried() {
    for (status, expected) in [
        (400, "invalid"),
        (401, "auth"),
        (403, "auth"),
        (404, "provider"),
        (422, "invalid"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let err = client(&server).complete(chat_request()).await.unwrap_err();
        let kind = match err {
            AiError::InvalidRequest(_) => "invalid",
            AiError::Auth => "auth",
            AiError::Provider { .. } => "provider",
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(kind, expected, "status {status}");
    }
}

#[tokio::test]
async fn a_slow_answer_times_out_and_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok().set_delay(Duration::from_millis(800)))
        .expect(1)
        .mount(&server)
        .await;
    let impatient = OpenAiClient::new(
        http_with(2, Duration::from_millis(150)),
        config(server.uri(), "m"),
    );
    assert_eq!(
        impatient.complete(chat_request()).await.unwrap_err(),
        AiError::Timeout
    );
}

#[tokio::test]
async fn an_unreachable_server_is_retried_then_reported_as_a_network_error() {
    let address = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let client = OpenAiClient::new(http(), config(format!("http://{address}"), "m"));
    assert!(matches!(
        client.complete(chat_request()).await,
        Err(AiError::Network(_))
    ));
}

#[tokio::test]
async fn a_garbled_success_body_is_a_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy</html>"))
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).complete(chat_request()).await,
        Err(AiError::Provider { status: 200, .. })
    ));
}
