//! Credentials: plain variables and secret *references*.
//!
//! Secret values never appear in the domain model or in the database (ADR-007): they live in
//! the OS vault and are addressed through a [`SecretRef`].

use crate::ids::{SecretId, VariableId};
use crate::EpochMs;

/// A non-secret configuration variable. `key` is unique across variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Identity.
    pub id: VariableId,
    /// Unique key (e.g. `OLLAMA_HOST`).
    pub key: String,
    /// The value. Not secret: it is stored in SQLite.
    pub value: String,
    /// What the variable is for.
    pub description: String,
}

/// Metadata of a secret held in the OS vault. Deliberately has no value field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    /// Identity; also the vault entry reference.
    pub id: SecretId,
    /// Unique display name (e.g. `OpenAI API key`).
    pub name: String,
    /// What the secret is for.
    pub description: String,
    /// When the secret was created.
    pub created_at: EpochMs,
}
