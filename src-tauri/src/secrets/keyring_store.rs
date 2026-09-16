//! OS-native secret storage built on the `keyring` crate (v3).
//!
//! The native platform backends - Windows Credential Manager, macOS Keychain,
//! and the Linux Secret Service - matching spec section 17. They are **not**
//! enabled by default: keyring v3 selects no backend unless a feature asks for
//! one and silently falls back to an in-process `mock` store that never
//! persists a secret. The features are therefore selected per platform in
//! `Cargo.toml` (`windows-native`, `apple-native`, `sync-secret-service`); if
//! `set` ever appears to succeed while `get` reports no entry, that feature
//! selection is the first thing to check.
//!
//! # Verifying against the real crate
//!
//! This module is the one place in the codebase whose correctness depends on a
//! third-party API that could not be compiled in the environment where it was
//! written (no Rust toolchain available). The calls used below are the
//! simplest keyring v3 shapes:
//!
//! ```text
//! Entry::new(service, user) -> keyring::Result<Entry>
//! entry.set_password(&str)  -> keyring::Result<()>
//! entry.get_password()      -> keyring::Result<String>
//! entry.delete_credential() -> keyring::Result<()>
//! keyring::Error::NoEntry
//! ```
//!
//! TODO(first compile): confirm those signatures and the `NoEntry` variant
//! against the exact keyring 3.x version cargo resolves - v3 had minor API
//! churn between releases (`delete_password` was renamed to
//! `delete_credential`). If a signature differs, only this file changes.
//!
//! No test here touches the real OS credential store: tests use
//! [`crate::secrets::memory::InMemorySecretStore`] so a test run can never
//! clobber a developer's actual keyring entries.

use keyring::Entry;

use super::{SecretStore, SecretStoreError};

/// Service name all application credentials are stored under. Using a stable,
/// product-specific name keeps our entries unambiguously ours in the OS store
/// (spec section 17).
pub const SERVICE_NAME: &str = "ai-coding-workspace";

/// [`SecretStore`] implementation over the platform keyring.
///
/// Each provider profile gets one keyring entry: service = [`SERVICE_NAME`],
/// account = provider profile id (spec sections 5, 17).
pub struct KeyringStore;

impl KeyringStore {
    /// Create the store. Cheap: no keyring access happens until a secret is
    /// read or written.
    pub fn new() -> Self {
        Self
    }

    fn entry(&self, account: &str) -> Result<Entry, SecretStoreError> {
        if account.trim().is_empty() {
            return Err(SecretStoreError::InvalidAccount);
        }
        Entry::new(SERVICE_NAME, account).map_err(platform_error)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for KeyringStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyringStore")
            .field("service", &SERVICE_NAME)
            .finish()
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            // A missing entry simply means no secret saved for this provider.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(platform_error(error)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        let entry = self.entry(account)?;
        // TODO(first compile): confirm `set_password` on the resolved v3.
        entry.set_password(secret).map_err(platform_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        let entry = self.entry(account)?;
        // TODO(first compile): confirm `delete_credential` (v3) vs
        // `delete_password` (v2) on the resolved version.
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Deleting an absent secret is treated as success.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(platform_error(error)),
        }
    }
}

/// Map a keyring error to [`SecretStoreError`] without ever touching the
/// secret: only the platform error text is carried, and the keyring/OS errors
/// describe the *credential operation*, not the credential value (spec
/// sections 5, 16).
fn platform_error(error: keyring::Error) -> SecretStoreError {
    SecretStoreError::Platform(error.to_string())
}

/// Tests for the real OS-backed store.
///
/// **Nothing here writes to the OS credential store.** A `set` would create (or
/// overwrite) a real entry in the developer's Windows Credential Manager, and a
/// `delete` could remove one - so this suite only exercises the read path and
/// the account validation that runs before any OS call. The full
/// set/get/delete contract is covered against
/// [`crate::secrets::memory::InMemorySecretStore`], and the command layer's
/// handling of a *failing* store is covered in `crate::commands::providers`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::unique_suffix;

    #[test]
    fn an_account_without_a_stored_secret_is_reported_as_absent() {
        // Spec section 16: "Secure credential storage errors" - a provider
        // whose key was never saved is the *normal* case (a gateway may need no
        // credential), so it must not surface as a failure. Reading a name that
        // cannot exist is non-mutating, which is what makes it safe to check
        // against the real backend.
        let store = KeyringStore::new();
        let account = format!(
            "test-account-that-does-not-exist-{}-{}",
            std::process::id(),
            unique_suffix()
        );

        match store.get(&account) {
            // Windows Credential Manager and the macOS Keychain report "no such
            // entry", which is not an error.
            Ok(secret) => assert_eq!(secret, None, "an unknown account must report no secret"),
            // A platform whose credential store is unreachable (a headless
            // Linux box with no Secret Service) reports a *classified* error
            // instead of inventing a secret or panicking. Not asserted equal:
            // this machine cannot produce that case.
            Err(SecretStoreError::Platform(message)) => {
                assert!(!message.is_empty());
                println!("no OS credential store available on this machine: {message}");
            }
            Err(other) => panic!("unexpected result for an absent account: {other:?}"),
        }

        // Reading is repeatable and never creates the entry.
        assert!(matches!(store.get(&account), Ok(None) | Err(SecretStoreError::Platform(_))));
    }

    #[test]
    fn empty_accounts_are_rejected_before_any_os_call() {
        // An empty account would address one shared entry in every backend, so
        // it is refused before the keyring is touched - which is also why this
        // test cannot mutate anything.
        let store = KeyringStore::new();

        for account in ["", "   ", "\t\n"] {
            assert!(
                matches!(
                    store.get(account),
                    Err(SecretStoreError::InvalidAccount)
                ),
                "get({account:?}) must be refused"
            );
            assert!(matches!(
                store.set(account, "sk-anything"),
                Err(SecretStoreError::InvalidAccount)
            ));
            assert!(matches!(
                store.delete(account),
                Err(SecretStoreError::InvalidAccount)
            ));
        }
    }

    #[test]
    fn errors_and_debug_output_never_contain_a_secret() {
        // The secret is handed to `set` and must not survive into any message,
        // even the one produced for a rejected account (spec sections 5, 17).
        let secret = "sk-DO-NOT-LEAK-7b2e40";
        let store = KeyringStore::new();

        let message = store.set("", secret).unwrap_err().to_string();
        assert!(
            !message.contains(secret),
            "the error text leaked the credential: {message}"
        );
        assert!(matches!(
            store.set("  ", secret),
            Err(SecretStoreError::InvalidAccount)
        ));

        let rendered = format!("{store:?}");
        assert!(!rendered.contains(secret), "Debug leaked the credential: {rendered}");
        assert!(
            rendered.contains(SERVICE_NAME),
            "the description should name the service it addresses: {rendered}"
        );
        assert_eq!(
            store.get("").unwrap_err().to_string(),
            "secret account name must not be empty"
        );
    }
}
