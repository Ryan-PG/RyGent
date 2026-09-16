//! Process-local [`SecretStore`] implementation.
//!
//! Compiled in all profiles (not just `cfg(test)`) for two reasons:
//!
//! 1. It is what the Milestone 2 unit tests exercise, so the trait contract is
//!    covered without touching the developer's real OS keyring.
//! 2. It is the drop-in backend for environments where no OS credential store
//!    is reachable (for example a Linux box without a Secret Service), so that
//!    only the wiring in [`crate::run`] has to change.
//!
//! Nothing here is persisted: secrets live until the process exits.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use super::{SecretStore, SecretStoreError};

/// In-memory secret store keyed by account (provider id).
#[derive(Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored secrets. Existence only - no secret material.
    pub fn len(&self) -> usize {
        self.secrets
            .lock()
            .map(|secrets| secrets.len())
            .unwrap_or(0)
    }

    /// Whether no secret is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>, SecretStoreError> {
        self.secrets.lock().map_err(|_| {
            SecretStoreError::Platform("in-memory secret store lock was poisoned".to_string())
        })
    }
}

/// Manual `Debug` so a stray `dbg!`/log line can never print secret values.
impl fmt::Debug for InMemorySecretStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accounts: Vec<String> = self
            .secrets
            .lock()
            .map(|secrets| secrets.keys().cloned().collect())
            .unwrap_or_default();
        formatter
            .debug_struct("InMemorySecretStore")
            .field("accounts", &accounts)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
        validate_account(account)?;
        let secrets = self.lock()?;
        Ok(secrets.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        validate_account(account)?;
        let mut secrets = self.lock()?;
        secrets.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        validate_account(account)?;
        let mut secrets = self.lock()?;
        secrets.remove(account);
        Ok(())
    }
}

/// Reject empty accounts early: the OS keyring APIs reject them too, and an
/// empty account would silently address a single shared entry.
fn validate_account(account: &str) -> Result<(), SecretStoreError> {
    if account.trim().is_empty() {
        return Err(SecretStoreError::InvalidAccount);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete_round_trip() {
        let store = InMemorySecretStore::new();

        assert_eq!(store.get("provider-a").unwrap(), None);

        store.set("provider-a", "sk-test-key-a").unwrap();
        assert_eq!(
            store.get("provider-a").unwrap(),
            Some("sk-test-key-a".to_string())
        );

        store.delete("provider-a").unwrap();
        assert_eq!(store.get("provider-a").unwrap(), None);
    }

    #[test]
    fn secrets_are_isolated_per_account() {
        let store = InMemorySecretStore::new();
        store.set("provider-a", "key-a").unwrap();
        store.set("provider-b", "key-b").unwrap();

        assert_eq!(store.get("provider-a").unwrap(), Some("key-a".to_string()));
        assert_eq!(store.get("provider-b").unwrap(), Some("key-b".to_string()));
        assert_eq!(store.len(), 2);

        store.set("provider-a", "key-a-rotated").unwrap();
        assert_eq!(
            store.get("provider-a").unwrap(),
            Some("key-a-rotated".to_string())
        );
        assert_eq!(store.get("provider-b").unwrap(), Some("key-b".to_string()));
    }

    #[test]
    fn deleting_an_absent_secret_is_not_an_error() {
        let store = InMemorySecretStore::new();
        store.delete("never-existed").unwrap();
        store.delete("never-existed").unwrap();
    }

    #[test]
    fn empty_accounts_are_rejected() {
        let store = InMemorySecretStore::new();
        assert!(matches!(
            store.get("  "),
            Err(SecretStoreError::InvalidAccount)
        ));
        assert!(matches!(
            store.set("", "sk-test-key"),
            Err(SecretStoreError::InvalidAccount)
        ));
        assert!(matches!(
            store.delete(""),
            Err(SecretStoreError::InvalidAccount)
        ));
    }

    #[test]
    fn errors_and_debug_output_never_contain_secret_values() {
        let store = InMemorySecretStore::new();
        let secret = "sk-test-DO-NOT-LEAK-5f3a91";
        store.set("provider-a", secret).unwrap();

        let rendered = format!("{store:?}");
        assert!(
            !rendered.contains(secret),
            "Debug output leaked the secret: {rendered}"
        );
        assert!(rendered.contains("provider-a"), "account names are expected");

        // Empty-account errors are about the account, never about a value.
        let error = store.set("", secret).unwrap_err().to_string();
        assert!(!error.contains(secret), "error text leaked the secret: {error}");
    }
}
