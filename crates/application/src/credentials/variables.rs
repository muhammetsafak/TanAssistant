//! Use cases for non-secret variables.

use std::sync::Arc;

use tan_domain::{Variable, VariableId};

use super::{
    check_text, clean_line, CredentialsError, CredentialsResult, ValidationError,
    MAX_DESCRIPTION_LEN, MAX_VARIABLE_KEY_LEN, MAX_VARIABLE_VALUE_LEN,
};
use crate::ports::{IdGenerator, VariableRepository};

const KEY_CONFLICT: &str = "a variable with this key already exists";

/// Lists all variables ordered by key.
pub struct ListVariables<V> {
    variables: Arc<V>,
}

impl<V: VariableRepository> ListVariables<V> {
    /// Creates the use case.
    #[must_use]
    pub fn new(variables: Arc<V>) -> Self {
        Self { variables }
    }

    /// Runs it.
    pub async fn execute(&self) -> CredentialsResult<Vec<Variable>> {
        self.variables
            .list()
            .await
            .map_err(|e| CredentialsError::from_repository(e, KEY_CONFLICT))
    }
}

/// Input of [`UpsertVariable`].
#[derive(Debug, Clone)]
pub struct UpsertVariableInput {
    /// `None` creates a new variable; `Some` updates that one.
    pub id: Option<VariableId>,
    /// Unique key (`[A-Za-z0-9_.-]`, up to 128 bytes).
    pub key: String,
    /// The value (may be empty).
    pub value: String,
    /// What the variable is for.
    pub description: String,
}

/// Creates or updates a variable.
pub struct UpsertVariable<V, I> {
    variables: Arc<V>,
    ids: Arc<I>,
}

impl<V: VariableRepository, I: IdGenerator> UpsertVariable<V, I> {
    /// Creates the use case.
    #[must_use]
    pub fn new(variables: Arc<V>, ids: Arc<I>) -> Self {
        Self { variables, ids }
    }

    /// Validates the input and saves the variable.
    pub async fn execute(&self, input: UpsertVariableInput) -> CredentialsResult<Variable> {
        let key = clean_line("key", &input.key, MAX_VARIABLE_KEY_LEN)?;
        if !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(ValidationError::InvalidKeyCharacters.into());
        }
        check_text("value", &input.value, MAX_VARIABLE_VALUE_LEN)?;
        check_text("description", &input.description, MAX_DESCRIPTION_LEN)?;

        let id = match input.id {
            Some(id) => {
                let exists = self
                    .variables
                    .get(&id)
                    .await
                    .map_err(|e| CredentialsError::from_repository(e, KEY_CONFLICT))?
                    .is_some();
                if !exists {
                    return Err(CredentialsError::NotFound { entity: "variable" });
                }
                id
            }
            None => VariableId::new(self.ids.new_id()),
        };
        let variable = Variable {
            id,
            key,
            value: input.value,
            description: input.description.trim().to_owned(),
        };
        self.variables
            .upsert(&variable)
            .await
            .map_err(|e| CredentialsError::from_repository(e, KEY_CONFLICT))?;
        Ok(variable)
    }
}

/// Deletes a variable.
pub struct DeleteVariable<V> {
    variables: Arc<V>,
}

impl<V: VariableRepository> DeleteVariable<V> {
    /// Creates the use case.
    #[must_use]
    pub fn new(variables: Arc<V>) -> Self {
        Self { variables }
    }

    /// Deletes the variable; a missing one is [`CredentialsError::NotFound`].
    pub async fn execute(&self, id: &VariableId) -> CredentialsResult<()> {
        self.variables
            .delete(id)
            .await
            .map_err(|e| CredentialsError::from_repository(e, KEY_CONFLICT))
    }
}
