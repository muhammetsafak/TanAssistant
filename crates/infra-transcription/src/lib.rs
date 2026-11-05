//! Transcription adapter: local whisper and the model manager.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-transcription";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-transcription");
    }
}
