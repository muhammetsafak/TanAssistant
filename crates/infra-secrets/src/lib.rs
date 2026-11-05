//! Secrets adapter: OS keychain with an encrypted-file fallback.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-secrets";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-secrets");
    }
}
