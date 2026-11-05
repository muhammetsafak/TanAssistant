//! The API key must never reach an error message, a `Debug` print or a log line.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tan_application::ports::{AiError, ChatModel};
use tan_infra_ai::openai::OpenAiClient;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{chat_request, config, http, KEY};

#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedBuffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

// A current-thread runtime (the `#[tokio::test]` default) keeps every log call on the thread
// the thread-local subscriber is installed on.
#[tokio::test]
async fn logs_and_errors_never_contain_the_api_key() {
    let logs = SharedBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    // The provider echoes the full key, then a partial one, in its error text.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": { "message": format!("bad key {KEY} (partial: sk-test-****cdef)") },
        })))
        .expect(3)
        .mount(&server)
        .await;
    let client = OpenAiClient::new(http(), config(server.uri(), "m"));

    let err = client.complete(chat_request()).await.unwrap_err();

    let AiError::Provider { message, .. } = &err else {
        panic!("unexpected {err:?}");
    };
    assert!(message.contains("[REDACTED]"));
    for text in [
        message.clone(),
        err.to_string(),
        format!("{err:?}"),
        format!("{client:?}"),
    ] {
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(!text.contains("sk-test"), "{text}");
    }

    let captured = logs.text();
    assert!(
        captured.contains("AI request failed"),
        "log not captured: {captured}"
    );
    assert!(captured.contains("retrying"), "{captured}");
    assert!(!captured.contains(KEY), "{captured}");
    assert!(!captured.contains("0123456789abcdef"), "{captured}");
    assert!(!captured.contains("sk-test"), "{captured}");
    assert!(
        !captured.to_ascii_lowercase().contains("bearer"),
        "{captured}"
    );
}

#[tokio::test]
async fn auth_failures_do_not_echo_the_provider_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": format!("Incorrect API key provided: {KEY}") },
        })))
        .mount(&server)
        .await;
    let client = OpenAiClient::new(http(), config(server.uri(), "m"));
    let err = client.complete(chat_request()).await.unwrap_err();
    assert_eq!(err, AiError::Auth);
    assert!(!format!("{err} {err:?}").contains("sk-test"));
}
