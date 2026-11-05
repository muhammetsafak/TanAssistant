//! AI adapters: chat, vision and transcription providers.

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
