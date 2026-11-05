//! Capture adapters: audio and screen capture, per-platform.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-capture";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-capture");
    }
}
