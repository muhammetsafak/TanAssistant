//! Credentials and settings use cases against in-memory fakes.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use tan_application::credentials::{
    CreateSecret, CreateSecretInput, CredentialsError, DeleteSecret, DeleteVariable, ListSecrets,
    ListVariables, ResolveSecret, RevealSecret, SetMasterPassword, UnlockSecretStore, UpdateSecret,
    UpdateSecretInput, UpsertVariable, UpsertVariableInput, ValidationError, MAX_SECRET_VALUE_LEN,
};
use tan_application::ports::{
    Clock, IdGenerator, SecretBackend, SecretMetadataRepository, SecretStore, SecretStoreControl,
    SecretStoreError, SecretStoreResult, SecretStoreStatus, SecretValue, SettingsRepository,
    VariableRepository,
};
use tan_application::settings::{GetSettings, SettingsUseCaseError, UpdateSettings};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    AppSettings, EpochMs, SecretId, SecretRef, SettingsPatch, UiLanguage, Variable, VariableId,
};

const VALUE: &str = "sk-super-secret-value-123";

// ---- fakes -----------------------------------------------------------------------------

#[derive(Default)]
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    fn new_id(&self) -> String {
        format!("id-{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Default)]
struct FixedClock(AtomicI64);

impl Clock for FixedClock {
    fn now(&self) -> EpochMs {
        EpochMs::new(self.0.fetch_add(1, Ordering::SeqCst) + 1_000)
    }
}

#[derive(Default)]
struct MemVariables(Mutex<BTreeMap<String, Variable>>);

impl VariableRepository for MemVariables {
    async fn upsert(&self, variable: &Variable) -> RepositoryResult<()> {
        let mut map = self.0.lock().unwrap();
        if map
            .values()
            .any(|v| v.key == variable.key && v.id != variable.id)
        {
            return Err(RepositoryError::Conflict("key".to_owned()));
        }
        map.insert(variable.id.to_string(), variable.clone());
        Ok(())
    }

    async fn get(&self, id: &VariableId) -> RepositoryResult<Option<Variable>> {
        Ok(self.0.lock().unwrap().get(id.as_str()).cloned())
    }

    async fn get_by_key(&self, key: &str) -> RepositoryResult<Option<Variable>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .values()
            .find(|v| v.key == key)
            .cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<Variable>> {
        let mut all: Vec<_> = self.0.lock().unwrap().values().cloned().collect();
        all.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(all)
    }

    async fn delete(&self, id: &VariableId) -> RepositoryResult<()> {
        self.0
            .lock()
            .unwrap()
            .remove(id.as_str())
            .map(|_| ())
            .ok_or_else(|| RepositoryError::not_found("variable", id.as_str()))
    }
}

#[derive(Default)]
struct MemSecretMetadata(Mutex<BTreeMap<String, SecretRef>>);

impl SecretMetadataRepository for MemSecretMetadata {
    async fn create(&self, secret: &SecretRef) -> RepositoryResult<()> {
        let mut map = self.0.lock().unwrap();
        if map
            .values()
            .any(|s| s.name == secret.name || s.id == secret.id)
        {
            return Err(RepositoryError::Conflict("name".to_owned()));
        }
        map.insert(secret.id.to_string(), secret.clone());
        Ok(())
    }

    async fn update(&self, secret: &SecretRef) -> RepositoryResult<()> {
        let mut map = self.0.lock().unwrap();
        if map
            .values()
            .any(|s| s.name == secret.name && s.id != secret.id)
        {
            return Err(RepositoryError::Conflict("name".to_owned()));
        }
        match map.get_mut(secret.id.as_str()) {
            Some(slot) => {
                *slot = secret.clone();
                Ok(())
            }
            None => Err(RepositoryError::not_found("secret", secret.id.as_str())),
        }
    }

    async fn get(&self, id: &SecretId) -> RepositoryResult<Option<SecretRef>> {
        Ok(self.0.lock().unwrap().get(id.as_str()).cloned())
    }

    async fn list(&self) -> RepositoryResult<Vec<SecretRef>> {
        let mut all: Vec<_> = self.0.lock().unwrap().values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn delete(&self, id: &SecretId) -> RepositoryResult<()> {
        self.0
            .lock()
            .unwrap()
            .remove(id.as_str())
            .map(|_| ())
            .ok_or_else(|| RepositoryError::not_found("secret", id.as_str()))
    }
}

#[derive(Default)]
struct MemVault {
    values: Mutex<HashMap<String, String>>,
    fail_puts: AtomicBool,
    locked: AtomicBool,
}

impl MemVault {
    fn len(&self) -> usize {
        self.values.lock().unwrap().len()
    }

    fn raw(&self, id: &str) -> Option<String> {
        self.values.lock().unwrap().get(id).cloned()
    }
}

impl SecretStore for MemVault {
    async fn put(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        if self.fail_puts.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Backend("vault write failed".to_owned()));
        }
        self.values
            .lock()
            .unwrap()
            .insert(id.to_string(), value.expose().to_owned());
        Ok(())
    }

    async fn get(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        Ok(self.raw(id.as_str()).map(SecretValue::new))
    }

    async fn delete(&self, id: &SecretId) -> SecretStoreResult<()> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Locked);
        }
        self.values.lock().unwrap().remove(id.as_str());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct FakeControl {
    unlock_calls: Mutex<Vec<String>>,
    master_calls: Mutex<Vec<(String, Option<String>)>>,
}

impl SecretStoreControl for FakeControl {
    fn status(&self) -> SecretStoreStatus {
        SecretStoreStatus {
            backend: SecretBackend::EncryptedFile,
            available: true,
            locked: true,
            master_password_set: false,
        }
    }

    fn unlock(&self, password: &SecretValue) -> SecretStoreResult<()> {
        self.unlock_calls
            .lock()
            .unwrap()
            .push(password.expose().to_owned());
        Ok(())
    }

    fn set_master_password(
        &self,
        new_password: &SecretValue,
        current_password: Option<&SecretValue>,
    ) -> SecretStoreResult<()> {
        self.master_calls.lock().unwrap().push((
            new_password.expose().to_owned(),
            current_password.map(|p| p.expose().to_owned()),
        ));
        Ok(())
    }

    fn lock(&self) {}
}

struct Secrets {
    metadata: Arc<MemSecretMetadata>,
    vault: Arc<MemVault>,
    ids: Arc<SequentialIds>,
    clock: Arc<FixedClock>,
}

impl Secrets {
    fn new() -> Self {
        Self {
            metadata: Arc::default(),
            vault: Arc::default(),
            ids: Arc::default(),
            clock: Arc::default(),
        }
    }

    fn create(&self) -> CreateSecret<MemSecretMetadata, MemVault, SequentialIds, FixedClock> {
        CreateSecret::new(
            self.metadata.clone(),
            self.vault.clone(),
            self.ids.clone(),
            self.clock.clone(),
        )
    }

    fn update(&self) -> UpdateSecret<MemSecretMetadata, MemVault> {
        UpdateSecret::new(self.metadata.clone(), self.vault.clone())
    }

    fn delete(&self) -> DeleteSecret<MemSecretMetadata, MemVault> {
        DeleteSecret::new(self.metadata.clone(), self.vault.clone())
    }

    fn list(&self) -> ListSecrets<MemSecretMetadata> {
        ListSecrets::new(self.metadata.clone())
    }

    fn reveal(&self) -> RevealSecret<MemSecretMetadata, MemVault> {
        RevealSecret::new(self.metadata.clone(), self.vault.clone())
    }

    fn add(&self, name: &str, value: &str) -> tan_application::credentials::SecretSummary {
        block_on(self.create().execute(CreateSecretInput {
            name: name.to_owned(),
            description: "test".to_owned(),
            value: SecretValue::new(value),
        }))
        .unwrap()
    }
}

// ---- secrets ---------------------------------------------------------------------------

#[test]
fn list_secrets_exposes_metadata_and_never_the_value() {
    let fx = Secrets::new();
    fx.add("OpenAI key", VALUE);
    fx.add("Gemini key", "another-very-secret-value");

    let listed = block_on(fx.list().execute()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "Gemini key", "ordered by name");

    let json = serde_json::to_string(&listed).unwrap();
    assert!(
        !json.contains(VALUE),
        "value leaked into the listing: {json}"
    );
    assert!(!json.contains("another-very-secret-value"));

    // The shape itself has no value-like field.
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut keys: Vec<_> = parsed[0].as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["created_at", "description", "id", "name"]);
}

#[test]
fn create_secret_stores_metadata_in_the_database_and_the_value_in_the_vault() {
    let fx = Secrets::new();
    let summary = fx.add("  OpenAI key ", VALUE);
    assert_eq!(summary.name, "OpenAI key", "name is trimmed");
    assert_eq!(fx.vault.raw(&summary.id).as_deref(), Some(VALUE));

    let stored = fx
        .metadata
        .0
        .lock()
        .unwrap()
        .get(&summary.id)
        .cloned()
        .unwrap();
    assert_eq!(stored.name, "OpenAI key");
    // The metadata type has no value field at all; the debug form must not carry it either.
    assert!(!format!("{stored:?}").contains(VALUE));
}

#[test]
fn duplicate_secret_name_is_a_conflict_and_stores_no_extra_value() {
    let fx = Secrets::new();
    fx.add("OpenAI key", VALUE);
    let err = block_on(fx.create().execute(CreateSecretInput {
        name: "OpenAI key".to_owned(),
        description: String::new(),
        value: SecretValue::new("second"),
    }))
    .unwrap_err();
    assert!(matches!(err, CredentialsError::Conflict(_)), "{err}");
    assert_eq!(fx.vault.len(), 1);
}

#[test]
fn failed_vault_write_rolls_the_metadata_back() {
    let fx = Secrets::new();
    fx.vault.fail_puts.store(true, Ordering::SeqCst);
    let err = block_on(fx.create().execute(CreateSecretInput {
        name: "OpenAI key".to_owned(),
        description: String::new(),
        value: SecretValue::new(VALUE),
    }))
    .unwrap_err();
    assert!(matches!(err, CredentialsError::Store(_)));
    assert!(fx.metadata.0.lock().unwrap().is_empty(), "orphan metadata");
    assert!(!format!("{err} {err:?}").contains(VALUE));
}

#[test]
fn secret_input_is_validated() {
    let fx = Secrets::new();
    let attempt = |name: &str, value: String| {
        block_on(fx.create().execute(CreateSecretInput {
            name: name.to_owned(),
            description: String::new(),
            value: SecretValue::new(value),
        }))
        .unwrap_err()
    };
    assert!(matches!(
        attempt("   ", "v".to_owned()),
        CredentialsError::Invalid(ValidationError::Empty("name"))
    ));
    assert!(matches!(
        attempt("n", String::new()),
        CredentialsError::Invalid(ValidationError::Empty("value"))
    ));
    let too_long = "x".repeat(MAX_SECRET_VALUE_LEN + 1);
    let err = attempt("n", too_long.clone());
    assert!(matches!(
        err,
        CredentialsError::Invalid(ValidationError::TooLong { field: "value", .. })
    ));
    assert!(
        !format!("{err}").contains(&too_long),
        "error repeats the value"
    );
    assert!(matches!(
        attempt("bad\u{7}name", "v".to_owned()),
        CredentialsError::Invalid(ValidationError::ControlCharacters("name"))
    ));
    assert!(fx.metadata.0.lock().unwrap().is_empty());
    assert_eq!(fx.vault.len(), 0);
}

#[test]
fn update_secret_changes_name_description_and_value() {
    let fx = Secrets::new();
    let created = fx.add("OpenAI key", VALUE);

    let updated = block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new(created.id.clone()),
        name: Some("OpenAI prod".to_owned()),
        description: Some("production".to_owned()),
        value: Some(SecretValue::new("rotated-value")),
    }))
    .unwrap();
    assert_eq!(updated.name, "OpenAI prod");
    assert_eq!(updated.description, "production");
    assert_eq!(updated.created_at, created.created_at);
    assert_eq!(fx.vault.raw(&created.id).as_deref(), Some("rotated-value"));

    // Value-only update leaves metadata alone.
    block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new(created.id.clone()),
        name: None,
        description: None,
        value: Some(SecretValue::new("third")),
    }))
    .unwrap();
    assert_eq!(fx.vault.raw(&created.id).as_deref(), Some("third"));
    assert_eq!(
        block_on(fx.list().execute()).unwrap()[0].name,
        "OpenAI prod"
    );
}

#[test]
fn update_secret_rejects_empty_missing_and_clashing_requests() {
    let fx = Secrets::new();
    let first = fx.add("first", "v1");
    fx.add("second", "v2");

    let nothing = block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new(first.id.clone()),
        name: None,
        description: None,
        value: None,
    }))
    .unwrap_err();
    assert!(matches!(
        nothing,
        CredentialsError::Invalid(ValidationError::NothingToUpdate)
    ));

    let missing = block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new("nope"),
        name: Some("x".to_owned()),
        description: None,
        value: None,
    }))
    .unwrap_err();
    assert!(matches!(
        missing,
        CredentialsError::NotFound { entity: "secret" }
    ));

    let clash = block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new(first.id),
        name: Some("second".to_owned()),
        description: None,
        value: None,
    }))
    .unwrap_err();
    assert!(matches!(clash, CredentialsError::Conflict(_)));
}

#[test]
fn failed_value_update_restores_the_old_metadata() {
    let fx = Secrets::new();
    let created = fx.add("OpenAI key", VALUE);
    fx.vault.fail_puts.store(true, Ordering::SeqCst);

    let err = block_on(fx.update().execute(UpdateSecretInput {
        id: SecretId::new(created.id.clone()),
        name: Some("renamed".to_owned()),
        description: None,
        value: Some(SecretValue::new("new-value")),
    }))
    .unwrap_err();
    assert!(matches!(err, CredentialsError::Store(_)));
    assert_eq!(block_on(fx.list().execute()).unwrap()[0].name, "OpenAI key");
    assert_eq!(fx.vault.raw(&created.id).as_deref(), Some(VALUE));
}

#[test]
fn delete_secret_removes_metadata_and_value() {
    let fx = Secrets::new();
    let created = fx.add("OpenAI key", VALUE);
    block_on(fx.delete().execute(&SecretId::new(created.id.clone()))).unwrap();
    assert_eq!(block_on(fx.list().execute()).unwrap().len(), 0);
    assert_eq!(fx.vault.len(), 0);

    let again = block_on(fx.delete().execute(&SecretId::new(created.id))).unwrap_err();
    assert!(matches!(
        again,
        CredentialsError::NotFound { entity: "secret" }
    ));
}

#[test]
fn delete_keeps_the_metadata_when_the_vault_is_unreachable() {
    let fx = Secrets::new();
    let created = fx.add("OpenAI key", VALUE);
    fx.vault.locked.store(true, Ordering::SeqCst);

    let err = block_on(fx.delete().execute(&SecretId::new(created.id.clone()))).unwrap_err();
    assert!(matches!(
        err,
        CredentialsError::Store(SecretStoreError::Locked)
    ));
    assert_eq!(block_on(fx.list().execute()).unwrap().len(), 1);
    assert_eq!(fx.vault.raw(&created.id).as_deref(), Some(VALUE));
}

#[test]
fn reveal_and_resolve_return_the_value_for_known_secrets_only() {
    let fx = Secrets::new();
    let created = fx.add("OpenAI key", VALUE);
    let id = SecretId::new(created.id.clone());

    let revealed = block_on(fx.reveal().execute(&id)).unwrap();
    assert_eq!(revealed.expose(), VALUE);
    assert!(!format!("{revealed:?}").contains(VALUE));

    let resolver = ResolveSecret::new(fx.metadata.clone(), fx.vault.clone());
    assert_eq!(block_on(resolver.execute(&id)).unwrap().expose(), VALUE);

    let missing = block_on(fx.reveal().execute(&SecretId::new("nope"))).unwrap_err();
    assert!(matches!(
        missing,
        CredentialsError::NotFound { entity: "secret" }
    ));

    // Metadata present, vault entry gone (for instance wiped from the OS vault by the user).
    fx.vault.values.lock().unwrap().clear();
    let gone = block_on(fx.reveal().execute(&id)).unwrap_err();
    assert!(matches!(gone, CredentialsError::SecretValueMissing));
}

#[test]
fn master_password_use_cases_validate_before_reaching_the_store() {
    let control = Arc::new(FakeControl::default());
    let set = SetMasterPassword::new(control.clone());
    let unlock = UnlockSecretStore::new(control.clone());

    let short = set.execute(&SecretValue::new("short"), None).unwrap_err();
    assert!(matches!(
        short,
        CredentialsError::Invalid(ValidationError::PasswordTooShort { .. })
    ));
    assert!(!format!("{short}").contains("short"));
    assert!(matches!(
        unlock.execute(&SecretValue::new("")),
        Err(CredentialsError::Invalid(ValidationError::Empty(
            "password"
        )))
    ));
    assert!(control.master_calls.lock().unwrap().is_empty());
    assert!(control.unlock_calls.lock().unwrap().is_empty());

    set.execute(
        &SecretValue::new("long enough password"),
        Some(&SecretValue::new("old")),
    )
    .unwrap();
    unlock.execute(&SecretValue::new("x")).unwrap();
    assert_eq!(
        control.master_calls.lock().unwrap()[0],
        ("long enough password".to_owned(), Some("old".to_owned()))
    );
    assert_eq!(control.unlock_calls.lock().unwrap()[0], "x");
}

// ---- variables -------------------------------------------------------------------------

fn variable_input(id: Option<&str>, key: &str, value: &str) -> UpsertVariableInput {
    UpsertVariableInput {
        id: id.map(VariableId::new),
        key: key.to_owned(),
        value: value.to_owned(),
        description: "d".to_owned(),
    }
}

#[test]
fn variables_can_be_created_listed_updated_and_deleted() {
    let repo = Arc::new(MemVariables::default());
    let upsert = UpsertVariable::new(repo.clone(), Arc::new(SequentialIds::default()));
    let list = ListVariables::new(repo.clone());
    let delete = DeleteVariable::new(repo);

    let host = block_on(upsert.execute(variable_input(
        None,
        "OLLAMA_HOST",
        "http://localhost:11434",
    )))
    .unwrap();
    block_on(upsert.execute(variable_input(None, "app.name-1", ""))).unwrap();
    let listed = block_on(list.execute()).unwrap();
    assert_eq!(
        listed.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(),
        ["OLLAMA_HOST", "app.name-1"]
    );

    let changed = block_on(upsert.execute(variable_input(
        Some(host.id.as_str()),
        "OLLAMA_HOST",
        "http://example:11434",
    )))
    .unwrap();
    assert_eq!(changed.id, host.id);
    assert_eq!(changed.value, "http://example:11434");
    assert_eq!(block_on(list.execute()).unwrap().len(), 2);

    block_on(delete.execute(&host.id)).unwrap();
    assert!(matches!(
        block_on(delete.execute(&host.id)),
        Err(CredentialsError::NotFound { entity: "variable" })
    ));
}

#[test]
fn variable_input_is_validated() {
    let repo = Arc::new(MemVariables::default());
    let upsert = UpsertVariable::new(repo, Arc::new(SequentialIds::default()));

    assert!(matches!(
        block_on(upsert.execute(variable_input(None, "  ", "v"))),
        Err(CredentialsError::Invalid(ValidationError::Empty("key")))
    ));
    for bad in ["has space", "semi;colon", "ünïcode", "a/b", "x=y"] {
        assert!(
            matches!(
                block_on(upsert.execute(variable_input(None, bad, "v"))),
                Err(CredentialsError::Invalid(
                    ValidationError::InvalidKeyCharacters
                ))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        block_on(upsert.execute(variable_input(None, &"k".repeat(129), "v"))),
        Err(CredentialsError::Invalid(ValidationError::TooLong {
            field: "key",
            ..
        }))
    ));
    assert!(matches!(
        block_on(upsert.execute(variable_input(Some("missing"), "K", "v"))),
        Err(CredentialsError::NotFound { entity: "variable" })
    ));
}

#[test]
fn duplicate_variable_key_is_a_conflict() {
    let repo = Arc::new(MemVariables::default());
    let upsert = UpsertVariable::new(repo, Arc::new(SequentialIds::default()));
    block_on(upsert.execute(variable_input(None, "HOST", "a"))).unwrap();
    assert!(matches!(
        block_on(upsert.execute(variable_input(None, "HOST", "b"))),
        Err(CredentialsError::Conflict(_))
    ));
}

// ---- settings --------------------------------------------------------------------------

#[derive(Default)]
struct MemSettings {
    current: Mutex<AppSettings>,
    fail: AtomicBool,
}

impl SettingsRepository for MemSettings {
    async fn load(&self) -> RepositoryResult<AppSettings> {
        Ok(self.current.lock().unwrap().clone())
    }

    async fn update(&self, patch: &SettingsPatch) -> RepositoryResult<AppSettings> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(RepositoryError::storage(std::io::Error::other("disk gone")));
        }
        let mut current = self.current.lock().unwrap();
        let next = current
            .patched(patch)
            .map_err(|e| RepositoryError::Invalid(e.to_string()))?;
        *current = next.clone();
        Ok(next)
    }
}

#[test]
fn settings_use_cases_read_update_and_reject_bad_values() {
    let repo = Arc::new(MemSettings::default());
    let get = GetSettings::new(repo.clone());
    let update = UpdateSettings::new(repo.clone());

    assert_eq!(block_on(get.execute()).unwrap(), AppSettings::default());

    let next = block_on(update.execute(&SettingsPatch {
        ui_language: Some(UiLanguage::En),
        consent_notice_acknowledged: Some(true),
        ..SettingsPatch::default()
    }))
    .unwrap();
    assert_eq!(next.ui_language, UiLanguage::En);
    assert!(next.consent_notice_acknowledged);
    assert_eq!(block_on(get.execute()).unwrap(), next);

    let bad = block_on(update.execute(&SettingsPatch {
        frame_interval_ms: Some(1),
        ..SettingsPatch::default()
    }))
    .unwrap_err();
    assert!(matches!(bad, SettingsUseCaseError::Invalid(_)));
    assert_eq!(
        block_on(get.execute()).unwrap(),
        next,
        "rejected patch changed nothing"
    );

    repo.fail.store(true, Ordering::SeqCst);
    let storage = block_on(update.execute(&SettingsPatch::default())).unwrap_err();
    assert!(matches!(storage, SettingsUseCaseError::Storage(_)));
    assert!(
        !storage.to_string().contains("disk gone"),
        "user-facing text must not carry backend details"
    );
}
