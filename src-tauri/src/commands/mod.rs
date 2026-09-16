//! Tauri command handlers exposed to the React frontend (spec section 4).
//!
//! Handlers are registered in the `invoke_handler` chain in [`crate::run`].
//! They are intentionally thin: validation and business rules live in the
//! owning module (`providers`, `secrets`, `workspaces`, `sessions`), so the
//! command layer only marshals arguments, locks [`AppState`], and converts
//! errors to messages the UI can display.
//!
//! Two rules apply to every command here (spec sections 5, 16, 17):
//!
//! - No command may return a secret. The only secret-related command outputs
//!   are a boolean (`provider_secret_status`) and write/delete acknowledgements.
//! - No command may include a secret in an error message. Errors returned to
//!   the frontend are `Result<_, String>` where the string comes from the
//!   module error types, all of which are constructed without secret material.

pub mod providers;
pub mod sessions;
pub mod workspaces;

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use crate::persistence::Storage;
use crate::secrets::SecretStore;

/// State shared by all commands, managed by Tauri.
///
/// Holds the single SQLite [`Storage`] and the swappable [`SecretStore`]
/// backend. Commands reach it through `tauri::State<'_, Mutex<AppState>>`, so
/// the store can be replaced (keyring vs in-memory) purely in [`crate::run`]
/// without touching command code.
pub struct AppState {
    /// Application database (non-secret metadata only).
    pub storage: Storage,
    /// OS-native credential storage for provider API keys.
    pub secrets: Box<dyn SecretStore + Send + Sync>,
}

impl AppState {
    /// Assemble the application state.
    pub fn new(storage: Storage, secrets: Box<dyn SecretStore + Send + Sync>) -> Self {
        Self { storage, secrets }
    }
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `dyn SecretStore` has no Debug bound, and the storage field must not
        // be formatted with its connection; keep this manual and minimal.
        formatter
            .debug_struct("AppState")
            .field("storage", &"<sqlite storage>")
            .field("secrets", &"<secret store>")
            .finish()
    }
}

/// Lock the shared application state, mapping a poisoned lock (a previous
/// panic while holding it) to a user-facing message instead of panicking again.
pub(crate) fn lock_state(state: &Mutex<AppState>) -> Result<MutexGuard<'_, AppState>, String> {
    state
        .lock()
        .map_err(|_| "the application state lock was poisoned by an earlier panic".to_string())
}

/// Convert any module error into the message string a command returns.
///
/// Centralised so it is obvious that command errors are plain, secret-free
/// text (spec section 16) - never a formatted request, header, or credential.
pub(crate) fn error_message(error: impl fmt::Display) -> String {
    error.to_string()
}
