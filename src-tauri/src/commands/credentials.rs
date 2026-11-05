//! IPC commands for variables, secrets and the secret store.
//!
//! Thin adapters: map DTOs to use-case input, call the use case, map the result back. Secret
//! values enter as plain strings (the IPC boundary), are moved straight into a `SecretValue`
//! and leave only through [`reveal_secret`]. Input DTOs deliberately do not derive `Debug`, and
//! every command skips its arguments in tracing spans.

use serde::{Deserialize, Serialize};
use specta::Type;
use tan_application::credentials::{
    CreateSecretInput, SecretSummary, UpdateSecretInput, UpsertVariableInput,
};
use tan_application::ports::{SecretBackend, SecretStoreStatus, SecretValue};
use tan_domain::{SecretId, Variable, VariableId};
use tauri::State;

use crate::error::{ErrorCode, IpcError};
use crate::state::AppState;

/// A non-secret variable.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VariableDto {
    /// Identity.
    pub id: String,
    /// Unique key.
    pub key: String,
    /// The value (not secret).
    pub value: String,
    /// What it is for.
    pub description: String,
}

impl From<Variable> for VariableDto {
    fn from(variable: Variable) -> Self {
        let Variable {
            id,
            key,
            value,
            description,
        } = variable;
        Self {
            id: id.to_string(),
            key,
            value,
            description,
        }
    }
}

/// Input to create or update a variable.
#[derive(Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UpsertVariableDto {
    /// Omit to create; set to update that variable.
    pub id: Option<String>,
    /// Unique key.
    pub key: String,
    /// The value.
    pub value: String,
    /// What it is for.
    pub description: String,
}

/// Secret metadata. There is intentionally no value field: values only leave the backend
/// through `reveal_secret`.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SecretDto {
    /// Identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What the secret is for.
    pub description: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: f64,
}

impl From<SecretSummary> for SecretDto {
    fn from(summary: SecretSummary) -> Self {
        let SecretSummary {
            id,
            name,
            description,
            created_at,
        } = summary;
        Self {
            id,
            name,
            description,
            // Epoch milliseconds stay far below 2^53, so the conversion is exact. Specta
            // refuses 64-bit integers, which is why this is an f64 (a JS number) on the wire.
            #[allow(clippy::cast_precision_loss)]
            created_at: created_at as f64,
        }
    }
}

/// Input to create a secret.
#[derive(Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreateSecretDto {
    /// Unique display name.
    pub name: String,
    /// What the secret is for.
    pub description: String,
    /// The value to keep in the vault.
    pub value: String,
}

/// Input to update a secret; omitted (null) fields stay unchanged.
#[derive(Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSecretDto {
    /// The secret to change.
    pub id: String,
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// New description.
    #[serde(default)]
    pub description: Option<String>,
    /// New value.
    #[serde(default)]
    pub value: Option<String>,
}

/// Which backend holds secret values.
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackendDto {
    /// The OS vault.
    Keyring,
    /// The master-password-encrypted file.
    EncryptedFile,
}

/// State of the secret store.
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SecretStoreStatusDto {
    /// Active backend.
    pub backend: SecretBackendDto,
    /// Whether the backend can work on this machine.
    pub available: bool,
    /// Whether values are unreachable until the master password is entered.
    pub locked: bool,
    /// Whether a master password has been set (always false for the OS vault).
    pub master_password_set: bool,
}

impl From<SecretStoreStatus> for SecretStoreStatusDto {
    fn from(status: SecretStoreStatus) -> Self {
        Self {
            backend: match status.backend {
                SecretBackend::Keyring => SecretBackendDto::Keyring,
                SecretBackend::EncryptedFile => SecretBackendDto::EncryptedFile,
            },
            available: status.available,
            locked: status.locked,
            master_password_set: status.master_password_set,
        }
    }
}

/// Lists all variables.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_variables(state: State<'_, AppState>) -> Result<Vec<VariableDto>, IpcError> {
    let variables = state.list_variables.execute().await?;
    Ok(variables.into_iter().map(VariableDto::from).collect())
}

/// Creates or updates a variable.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn upsert_variable(
    state: State<'_, AppState>,
    input: UpsertVariableDto,
) -> Result<VariableDto, IpcError> {
    let variable = state
        .upsert_variable
        .execute(UpsertVariableInput {
            id: input.id.map(VariableId::new),
            key: input.key,
            value: input.value,
            description: input.description,
        })
        .await?;
    Ok(variable.into())
}

/// Deletes a variable.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn delete_variable(state: State<'_, AppState>, id: String) -> Result<(), IpcError> {
    state.delete_variable.execute(&VariableId::new(id)).await?;
    Ok(())
}

/// Lists secret metadata (never values).
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn list_secrets(state: State<'_, AppState>) -> Result<Vec<SecretDto>, IpcError> {
    let secrets = state.list_secrets.execute().await?;
    Ok(secrets.into_iter().map(SecretDto::from).collect())
}

/// Creates a secret.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn create_secret(
    state: State<'_, AppState>,
    input: CreateSecretDto,
) -> Result<SecretDto, IpcError> {
    let summary = state
        .create_secret
        .execute(CreateSecretInput {
            name: input.name,
            description: input.description,
            value: SecretValue::new(input.value),
        })
        .await?;
    Ok(summary.into())
}

/// Updates a secret's name, description and/or value.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn update_secret(
    state: State<'_, AppState>,
    input: UpdateSecretDto,
) -> Result<SecretDto, IpcError> {
    let summary = state
        .update_secret
        .execute(UpdateSecretInput {
            id: SecretId::new(input.id),
            name: input.name,
            description: input.description,
            value: input.value.map(SecretValue::new),
        })
        .await?;
    Ok(summary.into())
}

/// Deletes a secret.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn delete_secret(state: State<'_, AppState>, id: String) -> Result<(), IpcError> {
    state.delete_secret.execute(&SecretId::new(id)).await?;
    Ok(())
}

/// Returns a secret's value. The one and only way a value leaves the backend: call it only
/// when the user explicitly asks to show or copy the secret.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn reveal_secret(state: State<'_, AppState>, id: String) -> Result<String, IpcError> {
    let value = state.reveal_secret.execute(&SecretId::new(id)).await?;
    Ok(value.expose().to_owned())
}

/// Reports the secret store's backend and lock state.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[must_use]
// Tauri injects `State` by value.
#[allow(clippy::needless_pass_by_value)]
pub fn secret_store_status(state: State<'_, AppState>) -> SecretStoreStatusDto {
    state.secret_store_status.execute().into()
}

/// Unlocks the encrypted-file secret store with the master password.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn unlock_secret_store(
    state: State<'_, AppState>,
    password: String,
) -> Result<SecretStoreStatusDto, IpcError> {
    let unlock = state.unlock_secret_store.clone();
    let password = SecretValue::new(password);
    // Argon2 key derivation is deliberately slow: keep it off the async runtime threads.
    tauri::async_runtime::spawn_blocking(move || unlock.execute(&password))
        .await
        .map_err(|e| join_error(&e))??;
    Ok(state.secret_store_status.execute().into())
}

/// Sets the master password of the encrypted-file store (first time), or changes it when
/// `currentPassword` is given.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn set_master_password(
    state: State<'_, AppState>,
    new_password: String,
    current_password: Option<String>,
) -> Result<SecretStoreStatusDto, IpcError> {
    let set = state.set_master_password.clone();
    let new_password = SecretValue::new(new_password);
    let current_password = current_password.map(SecretValue::new);
    tauri::async_runtime::spawn_blocking(move || {
        set.execute(&new_password, current_password.as_ref())
    })
    .await
    .map_err(|e| join_error(&e))??;
    Ok(state.secret_store_status.execute().into())
}

fn join_error(error: &tauri::Error) -> IpcError {
    tracing::error!(%error, "blocking task failed");
    IpcError::new(ErrorCode::Internal, "the operation was interrupted")
}
