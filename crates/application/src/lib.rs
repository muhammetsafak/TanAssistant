//! Application layer: use cases and ports (traits). Depends only on `tan-domain`.

pub mod ai;
pub mod credentials;
pub mod error;
pub mod ports;
pub mod processing;
pub mod recording;
pub mod sessions;
pub mod settings;
pub mod transcription;

pub use error::{RepositoryError, RepositoryResult};

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
