//! Application layer: use cases and ports (traits). Depends only on `tan-domain`.

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "application";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "application");
    }
}
