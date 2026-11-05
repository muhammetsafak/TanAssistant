//! Storage adapter: SQLite repositories and the data directory.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-storage";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-storage");
    }
}
