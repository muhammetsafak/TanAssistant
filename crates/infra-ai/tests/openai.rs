//! `OpenAI` adapter against a mock server.

mod common;

use std::time::Duration;

use serde_json::json;
use tan_application::ports::{
    AiError, AudioSource, ChatModel, TranscriptionModel, TranscriptionRequest, VisionModel,
};
use tan_domain::LanguageCode;
use tan_infra_ai::openai::OpenAiClient;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::{b64, chat_request, config, http, schema, vision_request, KEY, PNG};

fn client(server: &MockServer, model: &str) -> OpenAiClient {
    OpenAiClient::new(http(), config(format!("{}/v1", server.uri()), model))
}

fn completion(text: &str) -> serde_json::Value {
    json!({
        "choices": [{ "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 },
    })
}

#[tokio::test]
async fn chat_sends_bearer_auth_and_maps_the_reply() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(body_partial_json(json!({
            "model": "gpt-x",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hello" },
            ],
            "max_completion_tokens": 100,
            "temperature": 0.2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("hi there")))
        .expect(1)
        .mount(&server)
        .await;

    let reply = client(&server, "gpt-x")
        .complete(chat_request())
        .await
        .unwrap();
    assert_eq!(reply.text, "hi there");
    let usage = reply.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
    assert!(!reply.truncated);
}

#[tokio::test]
async fn length_finish_reason_marks_the_reply_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "cut o" }, "finish_reason": "length" }],
        })))
        .mount(&server)
        .await;
    let reply = client(&server, "m").complete(chat_request()).await.unwrap();
    assert!(reply.truncated);
    assert!(reply.usage.is_none());
}

#[tokio::test]
async fn structured_output_uses_a_strict_json_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "notes", "strict": true, "schema": { "type": "object" } },
            },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("{\"summary\":\"x\"}")))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = chat_request();
    request.json_schema = Some(schema());
    assert_eq!(
        client(&server, "m").complete(request).await.unwrap().text,
        "{\"summary\":\"x\"}"
    );
}

#[tokio::test]
async fn an_invalid_schema_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let mut request = chat_request();
    let mut bad = schema();
    bad.schema_json = "not json".to_owned();
    request.json_schema = Some(bad);
    let err = client(&server, "m").complete(request).await.unwrap_err();
    assert!(matches!(err, AiError::InvalidRequest(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn vision_sends_images_as_data_uris() {
    let server = MockServer::start().await;
    let data_uri = format!("data:image/png;base64,{}", b64(PNG));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "what is on the slide?" },
                    { "type": "image_url", "image_url": { "url": data_uri } },
                ],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("a title slide")))
        .expect(1)
        .mount(&server)
        .await;
    let reply = client(&server, "gpt-x")
        .describe(vision_request())
        .await
        .unwrap();
    assert_eq!(reply.text, "a title slide");
}

#[tokio::test]
async fn vision_rejects_unknown_image_types_and_empty_input() {
    let server = MockServer::start().await;
    let mut request = vision_request();
    request.images[0].mime_type = "image/bmp".to_owned();
    assert!(matches!(
        client(&server, "m").describe(request).await,
        Err(AiError::InvalidRequest(_))
    ));
    let mut request = vision_request();
    request.images.clear();
    assert!(matches!(
        client(&server, "m").describe(request).await,
        Err(AiError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn unauthorised_maps_to_auth_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": format!("Incorrect API key provided: {KEY}") },
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, "m")
        .complete(chat_request())
        .await
        .unwrap_err();
    assert_eq!(err, AiError::Auth);
    assert!(!err.to_string().contains(KEY));
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    // Longer than the client is willing to wait, so it is reported instead of slept through.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, "m")
        .complete(chat_request())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(120))
        }
    );
}

fn whisper_segments() -> serde_json::Value {
    json!({
        "text": "Merhaba. Toplantıya hoş geldiniz.",
        "language": "turkish",
        "duration": 7.5,
        "segments": [
            { "id": 0, "start": 0.0, "end": 2.4, "text": " Merhaba." },
            { "id": 1, "start": 2.4, "end": 7.5, "text": " Toplantıya hoş geldiniz." },
            { "id": 2, "start": 7.5, "end": 7.5, "text": "  " },
        ],
    })
}

fn wav_request(language: Option<&str>) -> TranscriptionRequest {
    TranscriptionRequest {
        audio: AudioSource::Bytes {
            file_name: "mic.wav".to_owned(),
            mime_type: "audio/wav".to_owned(),
            data: vec![0u8; 64],
        },
        language: language.map(|l| LanguageCode::new(l).unwrap()),
        duration_ms: Some(9_000),
        prompt: Some("Tan, Faz 06".to_owned()),
    }
}

fn body_contains(request: &Request, needle: &str) -> bool {
    String::from_utf8_lossy(&request.body).contains(needle)
}

#[tokio::test]
async fn whisper_transcription_requests_segments_and_maps_them() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(|r: &Request| {
            r.headers.get("content-type").is_some_and(|v| {
                v.to_str()
                    .unwrap_or_default()
                    .starts_with("multipart/form-data")
            }) && body_contains(r, "name=\"model\"\r\n\r\nwhisper-1")
                && body_contains(r, "name=\"response_format\"\r\n\r\nverbose_json")
                && body_contains(r, "name=\"timestamp_granularities[]\"\r\n\r\nsegment")
                && body_contains(r, "name=\"language\"\r\n\r\ntr")
                && body_contains(r, "name=\"prompt\"")
                && body_contains(r, "filename=\"mic.wav\"")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(whisper_segments()))
        .expect(1)
        .mount(&server)
        .await;

    let segments = client(&server, "whisper-1")
        .transcribe(wav_request(Some("tr-TR")))
        .await
        .unwrap();
    assert_eq!(segments.len(), 2, "the blank segment is dropped");
    assert_eq!((segments[0].start_ms, segments[0].end_ms), (0, 2_400));
    assert_eq!(segments[1].text, "Toplantıya hoş geldiniz.");
    assert_eq!(segments[0].language.as_ref().unwrap().as_str(), "tr-tr");
}

#[tokio::test]
async fn plain_json_models_yield_one_segment_spanning_the_audio() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(|r: &Request| {
            body_contains(r, "name=\"response_format\"\r\n\r\njson")
                && !body_contains(r, "timestamp_granularities")
                && !body_contains(r, "name=\"language\"")
                && !body_contains(r, "languages[]")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "Hello there",
            "languages": [{ "code": "en" }],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let segments = client(&server, "gpt-transcribe")
        .transcribe(wav_request(None))
        .await
        .unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!((segments[0].start_ms, segments[0].end_ms), (0, 9_000));
    assert_eq!(segments[0].text, "Hello there");
}

#[tokio::test]
async fn transcription_reads_files_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("system.mp3");
    std::fs::write(&file, [1u8, 2, 3]).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(|r: &Request| {
            body_contains(r, "filename=\"system.mp3\"") && body_contains(r, "audio/mpeg")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "ok" })))
        .expect(1)
        .mount(&server)
        .await;
    let request = TranscriptionRequest {
        audio: AudioSource::Path(file),
        language: None,
        duration_ms: None,
        prompt: None,
    };
    assert_eq!(
        client(&server, "whisper-1")
            .transcribe(request)
            .await
            .unwrap()
            .len(),
        1
    );

    let missing = TranscriptionRequest {
        audio: AudioSource::Path(dir.path().join("nope.wav")),
        language: None,
        duration_ms: None,
        prompt: None,
    };
    assert!(matches!(
        client(&server, "whisper-1").transcribe(missing).await,
        Err(AiError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn oversized_audio_is_refused_locally() {
    let server = MockServer::start().await;
    let request = TranscriptionRequest {
        audio: AudioSource::Bytes {
            file_name: "long.wav".to_owned(),
            mime_type: "audio/wav".to_owned(),
            data: vec![0u8; 25 * 1024 * 1024 + 1],
        },
        language: None,
        duration_ms: None,
        prompt: None,
    };
    let err = client(&server, "whisper-1")
        .transcribe(request)
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::InvalidRequest(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn gpt_transcribe_gets_languages_instead_of_language() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(|r: &Request| {
            body_contains(r, "name=\"languages[]\"\r\n\r\ntr")
                && !body_contains(r, "name=\"language\"")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "Merhaba" })))
        .expect(1)
        .mount(&server)
        .await;
    let segments = client(&server, "gpt-transcribe")
        .transcribe(wav_request(Some("tr-TR")))
        .await
        .unwrap();
    assert_eq!(segments[0].text, "Merhaba");
}
