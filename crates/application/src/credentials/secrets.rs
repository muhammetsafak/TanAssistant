//! Use cases for secrets. Metadata goes to the database, values go to the [`SecretStore`].
//!
//! [`RevealSecret`] (the user explicitly asking to see a value) and [`ResolveSecret`] (other
//! services reading a key to call a provider) are separate on purpose: only the first is ever
//! exposed over IPC.

use std::sync::Arc;

use serde::Serialize;
use tan_domain::{SecretId, SecretRef};

use super::{
    check_text, clean_line, CredentialsError, CredentialsResult, ValidationError,
    MAX_DESCRIPTION_LEN, MAX_MASTER_PASSWORD_LEN, MAX_SECRET_NAME_LEN, MAX_SECRET_VALUE_LEN,
    MIN_MASTER_PASSWORD_CHARS,
};
use crate::ports::{
    Clock, IdGenerator, SecretMetadataRepository, SecretStore, SecretStoreControl,
    SecretStoreStatus, SecretValue,
};

const NAME_CONFLICT: &str = "a secret with this name already exists";

/// What a secret listing exposes: metadata only. There is intentionally no value field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretSummary {
    /// Secret id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What the secret is for.
    pub description: String,
    /// Creation time, milliseconds since the Unix epoch.
    pub created_at: i64,
}

impl From<&SecretRef> for SecretSummary {
    fn from(secret: &SecretRef) -> Self {
        let SecretRef {
            id,
            name,
            description,
            created_at,
        } = secret;
        Self {
            id: id.to_string(),
            name: name.clone(),
            description: description.clone(),
            created_at: created_at.get(),
        }
    }
}

fn check_secret_value(value: &SecretValue) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty("value"));
    }
    if value.len() > MAX_SECRET_VALUE_LEN {
        return Err(ValidationError::TooLong {
            field: "value",
            max: MAX_SECRET_VALUE_LEN,
        });
    }
    Ok(())
}

fn check_password(password: &SecretValue, enforce_minimum: bool) -> Result<(), ValidationError> {
    if password.is_empty() {
        return Err(ValidationError::Empty("password"));
    }
    if password.len() > MAX_MASTER_PASSWORD_LEN {
        return Err(ValidationError::TooLong {
            field: "password",
            max: MAX_MASTER_PASSWORD_LEN,
        });
    }
    if enforce_minimum && password.expose().chars().count() < MIN_MASTER_PASSWORD_CHARS {
        return Err(ValidationError::PasswordTooShort {
            min: MIN_MASTER_PASSWORD_CHARS,
        });
    }
    Ok(())
}

/// Lists secret metadata, ordered by name.
pub struct ListSecrets<M> {
    metadata: Arc<M>,
}

impl<M: SecretMetadataRepository> ListSecrets<M> {
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>) -> Self {
        Self { metadata }
    }

    /// Runs it. Values are never read.
    pub async fn execute(&self) -> CredentialsResult<Vec<SecretSummary>> {
        let secrets = self
            .metadata
            .list()
            .await
            .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?;
        Ok(secrets.iter().map(SecretSummary::from).collect())
    }
}

/// Input of [`CreateSecret`].
#[derive(Debug)]
pub struct CreateSecretInput {
    /// Unique display name.
    pub name: String,
    /// What the secret is for.
    pub description: String,
    /// The value to store in the vault.
    pub value: SecretValue,
}

/// Creates a secret: metadata in the database, value in the vault.
pub struct CreateSecret<M, S, I, C> {
    metadata: Arc<M>,
    store: Arc<S>,
    ids: Arc<I>,
    clock: Arc<C>,
}

impl<M, S, I, C> CreateSecret<M, S, I, C>
where
    M: SecretMetadataRepository,
    S: SecretStore,
    I: IdGenerator,
    C: Clock,
{
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>, store: Arc<S>, ids: Arc<I>, clock: Arc<C>) -> Self {
        Self {
            metadata,
            store,
            ids,
            clock,
        }
    }

    /// Validates, stores the metadata, then the value; if the vault write fails the metadata
    /// is removed again so no secret exists without a value.
    pub async fn execute(&self, input: CreateSecretInput) -> CredentialsResult<SecretSummary> {
        let name = clean_line("name", &input.name, MAX_SECRET_NAME_LEN)?;
        check_text("description", &input.description, MAX_DESCRIPTION_LEN)?;
        check_secret_value(&input.value)?;

        let secret = SecretRef {
            id: SecretId::new(self.ids.new_id()),
            name,
            description: input.description.trim().to_owned(),
            created_at: self.clock.now(),
        };
        self.metadata
            .create(&secret)
            .await
            .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?;
        if let Err(e) = self.store.put(&secret.id, &input.value).await {
            // Best effort: the original failure is what the caller needs to see.
            let _ = self.metadata.delete(&secret.id).await;
            return Err(e.into());
        }
        Ok(SecretSummary::from(&secret))
    }
}

/// Input of [`UpdateSecret`]. `None` fields stay as they are.
#[derive(Debug)]
pub struct UpdateSecretInput {
    /// The secret to change.
    pub id: SecretId,
    /// New display name.
    pub name: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// New value to store in the vault.
    pub value: Option<SecretValue>,
}

/// Renames, re-describes and/or re-keys a secret.
pub struct UpdateSecret<M, S> {
    metadata: Arc<M>,
    store: Arc<S>,
}

impl<M: SecretMetadataRepository, S: SecretStore> UpdateSecret<M, S> {
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>, store: Arc<S>) -> Self {
        Self { metadata, store }
    }

    /// Applies the change. If the new value cannot be written the metadata change is undone.
    pub async fn execute(&self, input: UpdateSecretInput) -> CredentialsResult<SecretSummary> {
        if input.name.is_none() && input.description.is_none() && input.value.is_none() {
            return Err(ValidationError::NothingToUpdate.into());
        }
        let name = input
            .name
            .as_deref()
            .map(|n| clean_line("name", n, MAX_SECRET_NAME_LEN))
            .transpose()?;
        if let Some(description) = &input.description {
            check_text("description", description, MAX_DESCRIPTION_LEN)?;
        }
        if let Some(value) = &input.value {
            check_secret_value(value)?;
        }

        let existing = self
            .metadata
            .get(&input.id)
            .await
            .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?
            .ok_or(CredentialsError::NotFound { entity: "secret" })?;
        let updated = SecretRef {
            id: existing.id.clone(),
            name: name.unwrap_or_else(|| existing.name.clone()),
            description: input
                .description
                .as_deref()
                .map_or_else(|| existing.description.clone(), |d| d.trim().to_owned()),
            created_at: existing.created_at,
        };

        let metadata_changed = updated != existing;
        if metadata_changed {
            self.metadata
                .update(&updated)
                .await
                .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?;
        }
        if let Some(value) = &input.value {
            if let Err(e) = self.store.put(&updated.id, value).await {
                if metadata_changed {
                    let _ = self.metadata.update(&existing).await;
                }
                return Err(e.into());
            }
        }
        Ok(SecretSummary::from(&updated))
    }
}

/// Deletes a secret: the vault value first, then the metadata.
pub struct DeleteSecret<M, S> {
    metadata: Arc<M>,
    store: Arc<S>,
}

impl<M: SecretMetadataRepository, S: SecretStore> DeleteSecret<M, S> {
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>, store: Arc<S>) -> Self {
        Self { metadata, store }
    }

    /// Deletes the secret. If the vault cannot be reached the metadata stays, so the user
    /// can retry instead of leaving an orphaned vault entry behind.
    pub async fn execute(&self, id: &SecretId) -> CredentialsResult<()> {
        self.metadata
            .get(id)
            .await
            .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?
            .ok_or(CredentialsError::NotFound { entity: "secret" })?;
        self.store.delete(id).await?;
        self.metadata
            .delete(id)
            .await
            .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))
    }
}

async fn load_value<M: SecretMetadataRepository, S: SecretStore>(
    metadata: &M,
    store: &S,
    id: &SecretId,
) -> CredentialsResult<SecretValue> {
    metadata
        .get(id)
        .await
        .map_err(|e| CredentialsError::from_repository(e, NAME_CONFLICT))?
        .ok_or(CredentialsError::NotFound { entity: "secret" })?;
    store
        .get(id)
        .await?
        .ok_or(CredentialsError::SecretValueMissing)
}

/// Returns a secret's value because the user explicitly asked to see it ("show" / "copy").
pub struct RevealSecret<M, S> {
    metadata: Arc<M>,
    store: Arc<S>,
}

impl<M: SecretMetadataRepository, S: SecretStore> RevealSecret<M, S> {
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>, store: Arc<S>) -> Self {
        Self { metadata, store }
    }

    /// Reads the value.
    pub async fn execute(&self, id: &SecretId) -> CredentialsResult<SecretValue> {
        load_value(&*self.metadata, &*self.store, id).await
    }
}

/// Returns a secret's value for internal use by other services (for example an AI adapter
/// reading its API key). Never exposed over IPC.
pub struct ResolveSecret<M, S> {
    metadata: Arc<M>,
    store: Arc<S>,
}

impl<M: SecretMetadataRepository, S: SecretStore> ResolveSecret<M, S> {
    /// Creates the use case.
    #[must_use]
    pub fn new(metadata: Arc<M>, store: Arc<S>) -> Self {
        Self { metadata, store }
    }

    /// Reads the value.
    pub async fn execute(&self, id: &SecretId) -> CredentialsResult<SecretValue> {
        load_value(&*self.metadata, &*self.store, id).await
    }
}

/// Reports the secret store's state (backend, locked, master password set).
pub struct GetSecretStoreStatus<C> {
    control: Arc<C>,
}

impl<C: SecretStoreControl> GetSecretStoreStatus<C> {
    /// Creates the use case.
    #[must_use]
    pub fn new(control: Arc<C>) -> Self {
        Self { control }
    }

    /// Runs it.
    #[must_use]
    pub fn execute(&self) -> SecretStoreStatus {
        self.control.status()
    }
}

/// Unlocks the encrypted-file secret store. Key derivation is slow: call from a blocking
/// thread.
pub struct UnlockSecretStore<C> {
    control: Arc<C>,
}

impl<C: SecretStoreControl> UnlockSecretStore<C> {
    /// Creates the use case.
    #[must_use]
    pub fn new(control: Arc<C>) -> Self {
        Self { control }
    }

    /// Unlocks with `password`.
    pub fn execute(&self, password: &SecretValue) -> CredentialsResult<()> {
        check_password(password, false)?;
        self.control.unlock(password)?;
        Ok(())
    }
}

/// Sets or changes the master password of the encrypted-file secret store. Key derivation is
/// slow: call from a blocking thread.
pub struct SetMasterPassword<C> {
    control: Arc<C>,
}

impl<C: SecretStoreControl> SetMasterPassword<C> {
    /// Creates the use case.
    #[must_use]
    pub fn new(control: Arc<C>) -> Self {
        Self { control }
    }

    /// Sets the password (first time) or replaces it (`current_password` required).
    pub fn execute(
        &self,
        new_password: &SecretValue,
        current_password: Option<&SecretValue>,
    ) -> CredentialsResult<()> {
        check_password(new_password, true)?;
        if let Some(current) = current_password {
            check_password(current, false)?;
        }
        self.control
            .set_master_password(new_password, current_password)?;
        Ok(())
    }
}
