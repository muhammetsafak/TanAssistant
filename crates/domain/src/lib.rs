//! Domain layer: entities and rules. Depends on nothing.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "domain";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "domain");
    }
}
