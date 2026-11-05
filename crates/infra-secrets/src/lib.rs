//! Secrets adapter: OS keychain with an encrypted-file fallback (ADR-007).
//!
//! Wire-up: call [`open_secret_store`] with the data directory; it returns a
//! [`SelectedSecretStore`] that implements both `SecretStore` and `SecretStoreControl`.

// The port traits are async-shaped, but vault and file operations here are short synchronous
// calls, so the trait impls have no `.await`. Keep `async fn` so the impls stay valid if a call
// ever becomes genuinely async.
#![allow(clippy::unused_async_trait_impl)]

pub mod encrypted_file;
pub mod keyring_store;
pub mod select;

pub use encrypted_file::{EncryptedFileSecretStore, KdfParams};
pub use keyring_store::{KeyringSecretStore, SERVICE_NAME};
pub use select::{
    choose_backend, open_secret_store, open_secret_store_with, SelectedSecretStore,
    ENCRYPTED_FILE_NAME, PREFERENCE_FILE_NAME,
};

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
