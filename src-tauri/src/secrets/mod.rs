//! Secret storage abstraction (spec section 17).
//!
//! API keys and other credentials are stored only in the OS-native secure
//! credential mechanism (Windows Credential Manager, macOS Keychain, Linux
//! Secret Service) - never in plaintext SQLite fields (spec sections 5, 14).
//! The [`SecretStore`] trait keeps that machinery swappable and testable:
//!
//! - [`keyring_store::KeyringStore`] - the real OS-backed implementation.
//! - [`memory::InMemorySecretStore`] - process-local implementation used by
//!   tests (and available at runtime for future "no keyring available" flows).
//!
//! The trait is intentionally object safe so [`crate::commands::AppState`] can
//! hold a `Box<dyn SecretStore + Send + Sync>` and the backend can be swapped
//! without touching command code.

pub mod keyring_store;
pub mod memory;

/// Errors surfaced by a [`SecretStore`] implementation.
///
/// Deliberately opaque: platform keyring errors are stringified so the trait
/// does not depend on any particular crate's error type. Implementations must
/// ensure error text never contains secret material (spec sections 5, 16) -
/// which is why only the *operation* and platform detail are described here,
/// never the secret value itself.
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    /// The underlying OS credential storage failed.
    #[error("secure credential storage error: {0}")]
    Platform(String),

    /// An empty account (keyring "user") was requested.
    #[error("secret account name must not be empty")]
    InvalidAccount,
}

/// Abstraction over OS-native secure credential storage.
///
/// Credentials are addressed by `account`, which the application defines as
/// the provider profile id (one secret per provider, spec sections 5, 17).
pub trait SecretStore {
    /// Read the secret stored under `account`, or `None` if absent.
    fn get(&self, account: &str) -> Result<Option<String>, SecretStoreError>;

    /// Create or overwrite the secret stored under `account`.
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretStoreError>;

    /// Delete the secret stored under `account`. Deleting an absent secret is
    /// not an error, so callers can clean up unconditionally.
    fn delete(&self, account: &str) -> Result<(), SecretStoreError>;
}
