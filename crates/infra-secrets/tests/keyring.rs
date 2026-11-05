//! Round trip through the real OS vault.
//!
//! Uses a test-only service name and unique ids, and removes everything it created, so the
//! user's real entries are never touched. Where no vault is available on Linux/macOS (a headless CI
//! runner without a Secret Service) the tests skip themselves; on Windows that is a failure.

use futures_executor::block_on;
use tan_application::ports::{SecretStore, SecretStoreControl, SecretValue};
use tan_domain::SecretId;
use tan_infra_secrets::{KeyringSecretStore, SERVICE_NAME};

/// Cleans up its entries on drop, so a failing assertion cannot leave them behind.
struct Fixture {
    store: std::sync::Arc<KeyringSecretStore>,
    service: String,
    ids: Vec<SecretId>,
}

impl Fixture {
    // Returns `None` (skip) on Linux/macOS without a vault; on Windows it never does, which
    // is why clippy sees a needless `Option` there.
    #[cfg_attr(windows, allow(clippy::unnecessary_wraps))]
    fn new() -> Option<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let service = format!("{SERVICE_NAME}.test-{}-{nanos}", std::process::id());
        let store = std::sync::Arc::new(KeyringSecretStore::with_service(service.clone()));
        if store.is_available() {
            Some(Self {
                store,
                service,
                ids: Vec::new(),
            })
        } else {
            // Windows always has Credential Manager: a failing probe there is a regression, not
            // a missing vault, so it must fail loudly instead of passing silently.
            #[cfg(windows)]
            panic!("the Windows Credential Manager probe failed; the keyring adapter is broken");
            #[cfg(not(windows))]
            {
                eprintln!("skipping: no OS vault available on this machine");
                None
            }
        }
    }

    fn id(&mut self, name: &str) -> SecretId {
        let id = SecretId::new(name);
        self.ids.push(id.clone());
        id
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = block_on(self.store.delete(id));
        }
    }
}

#[test]
fn put_get_delete_round_trip() {
    let Some(mut fx) = Fixture::new() else { return };
    let id = fx.id("roundtrip");
    assert!(block_on(fx.store.get(&id)).unwrap().is_none());

    block_on(fx.store.put(&id, &SecretValue::new("sk-test-value-1"))).unwrap();
    let got = block_on(fx.store.get(&id)).unwrap().unwrap();
    assert_eq!(got.expose(), "sk-test-value-1");

    // Replacing keeps one entry with the new value.
    block_on(fx.store.put(&id, &SecretValue::new("sk-test-value-2"))).unwrap();
    assert_eq!(
        block_on(fx.store.get(&id)).unwrap().unwrap().expose(),
        "sk-test-value-2"
    );

    block_on(fx.store.delete(&id)).unwrap();
    assert!(block_on(fx.store.get(&id)).unwrap().is_none());
    block_on(fx.store.delete(&id)).unwrap(); // deleting a missing entry is fine
}

#[test]
fn unicode_and_multiline_values_survive() {
    let Some(mut fx) = Fixture::new() else { return };
    let id = fx.id("unicode");
    let value = "şifre-çğüöı\nline two\tTabbed";
    block_on(fx.store.put(&id, &SecretValue::new(value))).unwrap();
    assert_eq!(
        block_on(fx.store.get(&id)).unwrap().unwrap().expose(),
        value
    );
}

#[test]
fn status_reports_the_vault_without_a_master_password() {
    let Some(fx) = Fixture::new() else { return };
    let status = fx.store.status();
    assert!(status.available);
    assert!(!status.locked);
    assert!(!status.master_password_set);
}

/// Targets of the Credential Manager entries that belong to `service`.
#[cfg(windows)]
fn windows_entries_of(service: &str) -> Vec<String> {
    let output = std::process::Command::new("cmdkey")
        .arg("/list")
        .output()
        .expect("cmdkey is part of Windows");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("Target") && line.contains(service))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// Regression: unserialised concurrent vault calls used to leave entries behind that had been
/// reported as deleted. Many threads put/get/delete at once; nothing may remain afterwards.
#[test]
fn concurrent_use_leaves_nothing_behind() {
    const WORKERS: usize = 6;
    const ROUNDS: usize = 8;
    let Some(mut fx) = Fixture::new() else { return };
    let service = fx.service.clone();
    let store = fx.store.clone();
    // Register every id up front so the fixture's `Drop` cleans them up even if a worker
    // panics between its put and its delete.
    for worker in 0..WORKERS {
        for round in 0..ROUNDS {
            fx.id(&format!("concurrent-{worker}-{round}"));
        }
    }

    let threads: Vec<_> = (0..WORKERS)
        .map(|worker| {
            let store = store.clone();
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    let id = SecretId::new(format!("concurrent-{worker}-{round}"));
                    let value = format!("value-{worker}-{round}");
                    block_on(store.put(&id, &SecretValue::new(value.clone()))).unwrap();
                    let got = block_on(store.get(&id)).unwrap().unwrap();
                    assert_eq!(got.expose(), value);
                    block_on(store.delete(&id)).unwrap();
                    assert!(block_on(store.get(&id)).unwrap().is_none());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    for worker in 0..WORKERS {
        for round in 0..ROUNDS {
            let id = SecretId::new(format!("concurrent-{worker}-{round}"));
            assert!(
                block_on(store.get(&id)).unwrap().is_none(),
                "{id} survived its delete"
            );
        }
    }
    #[cfg(windows)]
    assert_eq!(
        windows_entries_of(&service),
        Vec::<String>::new(),
        "entries left in Credential Manager"
    );
    #[cfg(not(windows))]
    let _ = service;
}
