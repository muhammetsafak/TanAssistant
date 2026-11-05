//! Gemini adapter against a mock server.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{
    AiError, AudioSource, ChatModel, TranscriptionModel, TranscriptionRequest, VisionModel,
};
use tan_domain::LanguageCode;
use tan_infra_ai::gemini::{GeminiClient, MAX_INLINE_AUDIO_BYTES};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::{b64, chat_request, config, http, schema, vision_request, KEY, PNG};

fn client(server: &MockServer, model: &str) -> GeminiClient {
    GeminiClient::new(http(), config(format!("{}/v1beta", server.uri()), model))
}

fn generation(text: &str, finish: &str) -> serde_json::Value {
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "thinking", "thought": true }, { "text": text }] },
            "finishReason": finish,
        }],
        "usageMetadata": { "promptTokenCount": 9, "candidatesTokenCount": 4, "totalTokenCount": 13 },
    })
}

#[tokio::test]
async fn chat_authenticates_with_the_key_header_not_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-x:generateContent"))
        .and(header("x-goog-api-key", KEY))
        .and(|r: &Request| r.url.query().is_none())
        .and(body_partial_json(json!({
            "systemInstruction": { "parts": [{ "text": "be brief" }] },
            "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }],
            "generationConfig": { "temperature": 0.2, "maxOutputTokens": 100 },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(generation("hi", "STOP")))
        .expect(1)
        .mount(&server)
        .await;

    let reply = client(&server, "gemini-x")
        .complete(chat_request())
        .await
        .unwrap();
    assert_eq!(reply.text, "hi", "thought parts are skipped");
    let usage = reply.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (9, 4));
    assert!(!reply.truncated);
}

#[tokio::test]
async fn a_models_prefix_is_accepted_and_odd_names_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-x:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(generation("ok", "STOP")))
        .expect(1)
        .mount(&server)
        .await;
    client(&server, "models/gemini-x")
        .complete(chat_request())
        .await
        .unwrap();
    assert!(matches!(
        client(&server, "../evil?x=1")
            .complete(chat_request())
            .await,
        Err(AiError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn structured_output_uses_response_json_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseJsonSchema": { "type": "object" },
            },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(generation("{}", "MAX_TOKENS")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.json_schema = Some(schema());
    assert!(
        client(&server, "gemini-x")
            .complete(request)
            .await
            .unwrap()
            .truncated
    );
}

#[tokio::test]
async fn vision_sends_inline_data_parts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "inlineData": { "mimeType": "image/png", "data": b64(PNG) } },
                    { "text": "what is on the slide?" },
                ],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(generation("a slide", "STOP")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, "gemini-x")
            .describe(vision_request())
            .await
            .unwrap()
            .text,
        "a slide"
    );
}

#[tokio::test]
async fn a_blocked_prompt_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "promptFeedback": { "blockReason": "SAFETY" } })),
        )
        .mount(&server)
        .await;
    let err = client(&server, "gemini-x")
        .complete(chat_request())
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::InvalidRequest(m) if m.contains("SAFETY")));
}

#[tokio::test]
async fn an_invalid_key_answered_with_400_maps_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": 400,
                "message": "API key not valid. Please pass a valid API key.",
                "status": "INVALID_ARGUMENT",
            },
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, "gemini-x")
            .complete(chat_request())
            .await
            .unwrap_err(),
        AiError::Auth
    );
}

#[tokio::test]
async fn permission_denied_maps_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": 403, "message": "denied", "status": "PERMISSION_DENIED" },
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, "gemini-x")
            .complete(chat_request())
            .await
            .unwrap_err(),
        AiError::Auth
    );
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "60")
                .set_body_json(
                    json!({ "error": { "status": "RESOURCE_EXHAUSTED", "message": "quota" } }),
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server, "gemini-x")
            .complete(chat_request())
            .await
            .unwrap_err(),
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(60))
        }
    );
}

fn audio_request(bytes: usize, language: Option<&str>) -> TranscriptionRequest {
    TranscriptionRequest {
        audio: AudioSource::Bytes {
            file_name: "mic.wav".to_owned(),
            mime_type: "audio/wav".to_owned(),
            data: vec![7u8; bytes],
        },
        language: language.map(|l| LanguageCode::new(l).unwrap()),
        duration_ms: None,
        prompt: None,
    }
}

#[tokio::test]
async fn transcription_sends_audio_inline_and_parses_the_segments() {
    let server = MockServer::start().await;
    let reply = json!({
        "segments": [
            { "start": "00:00", "end": "00:04", "text": "Merhaba herkese." },
            { "start": "00:04", "end": "01:02", "text": "Bugün Faz 06'yı konuşacağız." },
            { "start": "bad", "end": "bad", "text": "dropped: no usable start" },
            { "start": "01:02", "end": "01:03", "text": "  " },
        ],
    });
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "contents": [{ "parts": [
                { "inlineData": { "mimeType": "audio/wav", "data": b64(&[7u8; 8]) } },
            ]}],
        })))
        .and(|r: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            body.pointer("/generationConfig/responseMimeType") == Some(&json!("application/json"))
                // Gemini 3 degrades below its default temperature, so none is sent.
                && body.pointer("/generationConfig/temperature").is_none()
                && body
                    .pointer("/contents/0/parts/1/text")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t.contains("MM:SS") && t.contains("\"tr\""))
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(generation(&reply.to_string(), "STOP")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let segments = client(&server, "gemini-x")
        .transcribe(audio_request(8, Some("tr")))
        .await
        .unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!((segments[0].start_ms, segments[0].end_ms), (0, 4_000));
    assert_eq!((segments[1].start_ms, segments[1].end_ms), (4_000, 62_000));
    assert_eq!(segments[1].language.as_ref().unwrap().as_str(), "tr");
}

#[tokio::test]
async fn oversized_audio_is_refused_locally() {
    let server = MockServer::start().await;
    let err = client(&server, "gemini-x")
        .transcribe(audio_request(MAX_INLINE_AUDIO_BYTES + 1, None))
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::InvalidRequest(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_cut_off_transcript_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(generation("{\"segments\":[", "MAX_TOKENS")),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server, "gemini-x")
            .transcribe(audio_request(8, None))
            .await,
        Err(AiError::Provider { .. })
    ));
}
