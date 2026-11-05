//! The factory: capability checks, secret resolution and wiring, with in-memory fakes.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tan_application::credentials::ResolveSecret;
use tan_application::ports::{
    AiError, ChatModel, ProviderFactory, SecretMetadataRepository, SecretStore, SecretStoreError,
    SecretStoreResult, SecretValue, TranscriptionModel, TranscriptionRequest, VisionModel,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{EpochMs, ProviderKind, ProviderProfile, ProviderProfileId, SecretId, SecretRef};
use tan_infra_ai::{AiProviderFactory, HttpSettings};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{chat_request, vision_request, KEY};

#[derive(Default)]
struct MemMetadata(Mutex<BTreeMap<String, SecretRef>>);

impl SecretMetadataRepository for MemMetadata {
    async fn create(&self, secret: &SecretRef) -> RepositoryResult<()> {
        self.0
            .lock()
            .unwrap()
            .insert(secret.id.to_string(), secret.clone());
        Ok(())
    }

    async fn update(&self, secret: &SecretRef) -> RepositoryResult<()> {
        self.create(secret).await
    }

    async fn get(&self, id: &SecretId) -> RepositoryResult<Option<SecretRef>> {
        Ok(self.0.lock().unwrap().get(id.as_str()).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<SecretRef>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }

    async fn delete(&self, id: &SecretId) -> RepositoryResult<()> {
        self.0
            .lock()
            .unwrap()
            .remove(id.as_str())
            .map(|_| ())
            .ok_or_else(|| RepositoryError::not_found("secret", id.as_str()))
    }
}

#[derive(Default)]
struct MemStore {
    values: Mutex<BTreeMap<String, String>>,
    locked: Mutex<bool>,
}

impl SecretStore for MemStore {
    async fn put(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        self.values
            .lock()
            .unwrap()
            .insert(id.to_string(), value.expose().to_owned());
        Ok(())
    }

    async fn get(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        if *self.locked.lock().unwrap() {
            return Err(SecretStoreError::Locked);
        }
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(id.as_str())
            .map(|v| SecretValue::new(v.clone())))
    }

    async fn delete(&self, id: &SecretId) -> SecretStoreResult<()> {
        self.values.lock().unwrap().remove(id.as_str());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}

struct Fixture {
    factory: AiProviderFactory<MemMetadata, MemStore>,
    store: Arc<MemStore>,
}

async fn fixture() -> Fixture {
    let metadata = Arc::new(MemMetadata::default());
    let store = Arc::new(MemStore::default());
    let id = SecretId::new("key-1");
    metadata
        .create(&SecretRef {
            id: id.clone(),
            name: "provider key".to_owned(),
            description: String::new(),
            created_at: EpochMs::new(1),
        })
        .await
        .unwrap();
    store.put(&id, &SecretValue::new(KEY)).await.unwrap();
    let resolve = Arc::new(ResolveSecret::new(metadata, store.clone()));
    let factory = AiProviderFactory::with_settings(resolve, HttpSettings::default()).unwrap();
    Fixture { factory, store }
}

fn profile(kind: ProviderKind, base_url: Option<String>, secret: Option<&str>) -> ProviderProfile {
    ProviderProfile {
        id: ProviderProfileId::new("p-1"),
        kind,
        display_name: "test".to_owned(),
        model: "model-x".to_owned(),
        base_url,
        secret_ref: secret.map(SecretId::new),
        supports_vision: false,
    }
}

#[tokio::test]
async fn builds_an_adapter_that_uses_the_resolved_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "wired" }, "finish_reason": "stop" }],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let fixture = fixture().await;
    let profile = profile(
        ProviderKind::OpenAi,
        Some(format!("{}/v1", server.uri())),
        Some("key-1"),
    );
    let chat = fixture.factory.chat(&profile).await.unwrap();
    assert_eq!(chat.complete(chat_request()).await.unwrap().text, "wired");
}

#[tokio::test]
async fn refuses_capabilities_the_provider_lacks() {
    let fixture = fixture().await;

    let deepseek = profile(ProviderKind::DeepSeek, None, Some("key-1"));
    assert!(fixture.factory.chat(&deepseek).await.is_ok());
    assert!(matches!(
        fixture.factory.vision(&deepseek).await,
        Err(AiError::Unsupported(_))
    ));
    assert!(matches!(
        fixture.factory.transcription(&deepseek).await,
        Err(AiError::Unsupported(_))
    ));

    let anthropic = profile(ProviderKind::Anthropic, None, Some("key-1"));
    assert!(fixture.factory.vision(&anthropic).await.is_ok());
    assert!(matches!(
        fixture.factory.transcription(&anthropic).await,
        Err(AiError::Unsupported(_))
    ));

    let openai = profile(ProviderKind::OpenAi, None, Some("key-1"));
    assert!(fixture.factory.transcription(&openai).await.is_ok());
    assert!(matches!(
        fixture.factory.catalog(&openai).await,
        Err(AiError::Unsupported(_))
    ));
}

#[tokio::test]
async fn the_unified_adapter_also_refuses_unsupported_calls() {
    let fixture = fixture().await;
    let chat_only = fixture
        .factory
        .chat(&profile(ProviderKind::DeepSeek, None, Some("key-1")))
        .await
        .unwrap();
    assert!(matches!(
        chat_only.describe(vision_request()).await,
        Err(AiError::Unsupported(_))
    ));
    let request = TranscriptionRequest {
        audio: tan_application::ports::AudioSource::Path("x.wav".into()),
        language: None,
        duration_ms: None,
        prompt: None,
    };
    assert!(matches!(
        chat_only.transcribe(request).await,
        Err(AiError::Unsupported(_))
    ));
}

#[tokio::test]
async fn key_problems_are_reported_without_a_network_call() {
    let fixture = fixture().await;

    let no_secret = profile(ProviderKind::OpenAi, None, None);
    assert!(matches!(
        fixture.factory.chat(&no_secret).await,
        Err(AiError::SecretUnavailable(_))
    ));

    let dangling = profile(ProviderKind::OpenAi, None, Some("missing"));
    assert!(matches!(
        fixture.factory.chat(&dangling).await,
        Err(AiError::SecretUnavailable(_))
    ));

    *fixture.store.locked.lock().unwrap() = true;
    let err = fixture
        .factory
        .chat(&profile(ProviderKind::OpenAi, None, Some("key-1")))
        .await
        .unwrap_err();
    assert!(matches!(&err, AiError::SecretUnavailable(m) if m.contains("locked")));
    assert!(!err.to_string().contains(KEY));
}

#[tokio::test]
async fn ollama_works_without_a_key_and_offers_a_catalog() {
    let fixture = fixture().await;
    let ollama = profile(
        ProviderKind::Ollama,
        Some("http://127.0.0.1:9".to_owned()),
        None,
    );
    assert!(fixture.factory.chat(&ollama).await.is_ok());
    assert!(fixture.factory.catalog(&ollama).await.is_ok());
    // A dangling key reference is not fatal where no key is needed.
    let dangling = profile(ProviderKind::Ollama, None, Some("missing"));
    assert!(fixture.factory.chat(&dangling).await.is_ok());
}

#[tokio::test]
async fn the_key_is_never_sent_over_plain_http_to_a_remote_host() {
    let fixture = fixture().await;
    for kind in [ProviderKind::OpenAi, ProviderKind::Ollama] {
        let remote = profile(
            kind,
            Some("http://ollama.example.com:11434".to_owned()),
            Some("key-1"),
        );
        assert!(
            matches!(
                fixture.factory.chat(&remote).await,
                Err(AiError::InvalidRequest(_))
            ),
            "{kind}"
        );
    }
    // Loopback http and remote https are fine; so is a key-less remote Ollama over http.
    let local = profile(
        ProviderKind::Ollama,
        Some("http://localhost:11434".to_owned()),
        Some("key-1"),
    );
    assert!(fixture.factory.chat(&local).await.is_ok());
    let secure = profile(
        ProviderKind::Ollama,
        Some("https://ollama.example.com".to_owned()),
        Some("key-1"),
    );
    assert!(fixture.factory.chat(&secure).await.is_ok());
    let keyless = profile(
        ProviderKind::Ollama,
        Some("http://10.0.0.5:11434".to_owned()),
        None,
    );
    assert!(fixture.factory.chat(&keyless).await.is_ok());
}

#[tokio::test]
async fn ollama_vision_follows_the_profile_flag() {
    let fixture = fixture().await;
    let mut ollama = profile(ProviderKind::Ollama, None, None);
    assert!(matches!(
        fixture.factory.vision(&ollama).await,
        Err(AiError::Unsupported(_))
    ));
    ollama.supports_vision = true;
    assert!(fixture.factory.vision(&ollama).await.is_ok());
}
