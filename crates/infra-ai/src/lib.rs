//! AI adapters: chat, vision and transcription providers (ADR-008).
//!
//! One adapter per provider, talking plain HTTPS through `reqwest` (no vendor SDKs). They share
//! [`http`] (timeouts, retry with exponential backoff, error classification, key redaction).
//! [`factory::AiProviderFactory`] is the entry point: it builds an adapter from a stored
//! profile and resolves the API key from the secret store. Default endpoints and suggested
//! model names live in [`defaults`] only.

pub mod anthropic;
pub mod deepseek;
pub mod defaults;
pub mod factory;
pub mod gemini;
pub mod http;
pub mod ollama;
pub mod openai;

pub use factory::{AiProvider, AiProviderFactory};
pub use http::{ClientConfig, HttpClient, HttpSettings};

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-ai";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-ai");
    }
}
