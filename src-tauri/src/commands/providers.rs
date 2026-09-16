//! Provider management commands (spec sections 5, 12, 17).
//!
//! The command surface behind the Providers panel:
//!
//! | command | purpose |
//! | --- | --- |
//! | [`list_providers`] | all profiles, metadata only |
//! | [`create_provider`] | insert a profile |
//! | [`update_provider`] | edit a profile (secret untouched) |
//! | [`delete_provider`] | remove a profile **and** its keyring entry |
//! | [`set_provider_secret`] | write (or clear) the keyring secret |
//! | [`provider_secret_status`] | whether a secret exists - never its value |
//! | [`test_provider`] | authenticated connectivity check |
//!
//! Nothing here returns secret material: the profile payloads come from the
//! secret-free [`ProviderRepository`], the status command returns a `bool`, and
//! the messages produced by [`error_message`] are built from error types that
//! never carry a credential (spec sections 5, 16, 17).

use std::sync::Mutex;

use tauri::State;

use crate::commands::{error_message, lock_state, AppState};
use crate::providers::{
    connectivity::{self, ProviderTestResult},
    ProviderInput, ProviderProfile, ProviderRepository,
};

/// List every configured provider profile, ordered for display.
#[tauri::command]
pub fn list_providers(state: State<'_, Mutex<AppState>>) -> Result<Vec<ProviderProfile>, String> {
    let app = lock_state(&state)?;
    ProviderRepository::new(&app.storage)
        .list()
        .map_err(error_message)
}

/// Create a provider profile. The API key is *not* part of this call - see
/// [`set_provider_secret`].
#[tauri::command]
pub fn create_provider(
    state: State<'_, Mutex<AppState>>,
    input: ProviderInput,
) -> Result<ProviderProfile, String> {
    let app = lock_state(&state)?;
    ProviderRepository::new(&app.storage)
        .create(&input)
        .map_err(error_message)
}

/// Update a provider profile's metadata. The stored secret is left untouched.
#[tauri::command]
pub fn update_provider(
    state: State<'_, Mutex<AppState>>,
    id: String,
    input: ProviderInput,
) -> Result<ProviderProfile, String> {
    let app = lock_state(&state)?;
    ProviderRepository::new(&app.storage)
        .update(&id, &input)
        .map_err(error_message)
}

/// Delete a provider profile and its keyring entry.
///
/// The secret is removed **first** and deliberately so: it is the sensitive
/// half, and deleting the profile row first would strand a credential in the OS
/// keyring if the keyring call then failed. If the keyring delete fails, the
/// profile is left intact so the user can retry and nothing is orphaned either
/// way. Workspace rows referencing this provider are kept (spec section 9).
#[tauri::command]
pub fn delete_provider(state: State<'_, Mutex<AppState>>, id: String) -> Result<(), String> {
    let app = lock_state(&state)?;
    delete_provider_in(&app, &id)
}

/// Store (or clear) the API key for a provider in the OS keyring.
///
/// An empty or whitespace-only `api_key` clears the stored credential, which is
/// how the UI's "Remove key" action is expressed - deleting the profile is not
/// required just to drop a key. The key is written to the keyring only: it is
/// never persisted in SQLite, never logged, and never echoed back (spec
/// sections 5, 14, 17).
#[tauri::command(rename_all = "camelCase")]
pub fn set_provider_secret(
    state: State<'_, Mutex<AppState>>,
    provider_id: String,
    api_key: String,
) -> Result<(), String> {
    let app = lock_state(&state)?;
    set_provider_secret_in(&app, &provider_id, &api_key)
}

/// Whether a secret is stored for this provider.
///
/// Existence only. The secret is read from the keyring and immediately reduced
/// to a `bool`, so the value never crosses the Tauri boundary (spec section 17).
#[tauri::command(rename_all = "camelCase")]
pub fn provider_secret_status(
    state: State<'_, Mutex<AppState>>,
    provider_id: String,
) -> Result<bool, String> {
    let app = lock_state(&state)?;
    provider_secret_status_in(&app, &provider_id)
}

/// Test that a provider's endpoint answers an authenticated request.
///
/// Returns a structured, secret-free result: the UI shows it inline instead of
/// an error dialog, because "HTTP 401" is a normal answer, not a crash
/// (spec sections 12, 16).
#[tauri::command(rename_all = "camelCase")]
pub async fn test_provider(
    state: State<'_, Mutex<AppState>>,
    provider_id: String,
) -> Result<ProviderTestResult, String> {
    // Read the endpoint and credential while the lock is held, then release it
    // before the network round-trip: a MutexGuard is not `Send`, and holding the
    // application state lock across an await would block every other command.
    let (base_url, api_key) = {
        let app = lock_state(&state)?;
        test_target_in(&app, &provider_id)?
    };

    Ok(connectivity::check(&base_url, api_key.as_deref()).await)
}

// --- plain functions behind the commands --------------------------------------
//
// Tauri state cannot be constructed in a unit test, so the body of each command
// that has behaviour lives in a function taking `&AppState` (the same seam
// `commands::workspaces` and `commands::sessions` use). The command is then a
// two-line wrapper, and error handling - including what a failing OS keyring
// looks like to the user - is testable (spec sections 16, 23).

/// [`delete_provider`] without the Tauri state wrapper.
pub(crate) fn delete_provider_in(app: &AppState, id: &str) -> Result<(), String> {
    let repository = ProviderRepository::new(&app.storage);

    if repository.find(id).map_err(error_message)?.is_none() {
        return Err(format!("provider not found: {id}"));
    }

    // Order matters: the credential goes first, so a keyring failure leaves the
    // profile (and therefore the reference to its secret) in place.
    app.secrets.delete(id).map_err(error_message)?;

    if !repository.delete(id).map_err(error_message)? {
        return Err(format!("provider not found: {id}"));
    }
    Ok(())
}

/// [`set_provider_secret`] without the Tauri state wrapper.
pub(crate) fn set_provider_secret_in(
    app: &AppState,
    provider_id: &str,
    api_key: &str,
) -> Result<(), String> {
    let repository = ProviderRepository::new(&app.storage);

    if repository
        .find(provider_id)
        .map_err(error_message)?
        .is_none()
    {
        return Err(format!("provider not found: {provider_id}"));
    }

    let api_key = api_key.trim();
    if api_key.is_empty() {
        return app.secrets.delete(provider_id).map_err(error_message);
    }
    app.secrets
        .set(provider_id, api_key)
        .map_err(error_message)
}

/// [`provider_secret_status`] without the Tauri state wrapper.
pub(crate) fn provider_secret_status_in(app: &AppState, provider_id: &str) -> Result<bool, String> {
    let repository = ProviderRepository::new(&app.storage);

    if repository
        .find(provider_id)
        .map_err(error_message)?
        .is_none()
    {
        return Err(format!("provider not found: {provider_id}"));
    }

    Ok(app
        .secrets
        .get(provider_id)
        .map_err(error_message)?
        .is_some())
}

/// The endpoint and credential [`test_provider`] needs, resolved from the
/// database and the keyring.
///
/// Split out so the "provider that is gone" and "credential store unavailable"
/// failures can be asserted without a network round trip; the key stays inside
/// this crate (spec section 17).
pub(crate) fn test_target_in(
    app: &AppState,
    provider_id: &str,
) -> Result<(String, Option<String>), String> {
    let repository = ProviderRepository::new(&app.storage);

    let profile = repository
        .find(provider_id)
        .map_err(error_message)?
        .ok_or_else(|| format!("provider not found: {provider_id}"))?;
    let api_key = app.secrets.get(&profile.id).map_err(error_message)?;

    Ok((profile.base_url, api_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Storage;
    use crate::secrets::memory::InMemorySecretStore;
    use crate::secrets::{SecretStore, SecretStoreError};

    /// A value that looks like a credential. Every test below asserts that it
    /// cannot appear in a payload, a message, or `Debug` output (spec 16/17).
    const KEY: &str = "sk-DO-NOT-LEAK-5f3a91";

    /// A store that always fails, standing in for the cases the OS keyring
    /// itself reports: no credential store on this machine, a locked keychain,
    /// a Secret Service that is not running (spec section 16: "Secure
    /// credential storage errors"). Used because the real backend must never be
    /// broken by a test.
    struct UnavailableSecretStore;

    impl SecretStore for UnavailableSecretStore {
        fn get(&self, _account: &str) -> Result<Option<String>, SecretStoreError> {
            Err(SecretStoreError::Platform(
                "the credential store is unavailable".to_string(),
            ))
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Platform(
                "the credential store is unavailable".to_string(),
            ))
        }

        fn delete(&self, _account: &str) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Platform(
                "the credential store is unavailable".to_string(),
            ))
        }
    }

    /// Application state shaped like `run()` builds it, with a swappable secret
    /// backend and an in-memory database.
    fn app_state(secrets: Box<dyn SecretStore + Send + Sync>) -> AppState {
        AppState::new(
            Storage::open_in_memory().expect("open an in-memory database"),
            secrets,
        )
    }

    /// A provider profile, plus the profile it created.
    fn provider(app: &AppState, name: &str) -> ProviderProfile {
        ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: name.to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .expect("create a provider profile")
    }

    #[test]
    fn a_stored_secret_is_only_ever_reported_as_existing() {
        let app = app_state(Box::new(InMemorySecretStore::new()));
        let profile = provider(&app, "Provider A");

        // Nothing stored yet.
        assert!(!provider_secret_status_in(&app, &profile.id).unwrap());

        set_provider_secret_in(&app, &profile.id, &format!("  {KEY}  ")).unwrap();
        assert!(provider_secret_status_in(&app, &profile.id).unwrap());

        // The key was stored trimmed, and the value stays in the secret layer:
        // what the frontend receives is a `bool`, and the profile payload the
        // provider list returns carries no credential (spec sections 5, 17).
        assert_eq!(
            app.secrets.get(&profile.id).unwrap().as_deref(),
            Some(KEY)
        );
        let listed = ProviderRepository::new(&app.storage).list().unwrap();
        let rendered = format!("{listed:?}");
        assert!(!rendered.contains(KEY), "the provider list leaked the key: {rendered}");
        assert_eq!(listed.len(), 1);

        // A blank key clears the stored secret (the UI's "Remove key").
        set_provider_secret_in(&app, &profile.id, "   ").unwrap();
        assert!(!provider_secret_status_in(&app, &profile.id).unwrap());
        assert_eq!(app.secrets.get(&profile.id).unwrap(), None);

        // Clearing twice is not an error.
        set_provider_secret_in(&app, &profile.id, "").unwrap();
    }

    #[test]
    fn provider_commands_report_an_unknown_provider_clearly() {
        let app = app_state(Box::new(InMemorySecretStore::new()));

        assert_eq!(
            set_provider_secret_in(&app, "no-such-provider", KEY).unwrap_err(),
            "provider not found: no-such-provider"
        );
        assert_eq!(
            provider_secret_status_in(&app, "no-such-provider").unwrap_err(),
            "provider not found: no-such-provider"
        );
        assert_eq!(
            delete_provider_in(&app, "no-such-provider").unwrap_err(),
            "provider not found: no-such-provider"
        );
        assert_eq!(
            test_target_in(&app, "no-such-provider").unwrap_err(),
            "provider not found: no-such-provider"
        );
        // None of those messages may echo the key that was passed in (spec 17).
        assert!(!set_provider_secret_in(&app, "no-such-provider", KEY)
            .unwrap_err()
            .contains(KEY));
    }

    #[test]
    fn an_unavailable_credential_store_is_reported_as_itself() {
        // Spec section 16: "Secure credential storage errors". The user must be
        // told that the credential store is the problem - not that the provider
        // is invalid, and not something about SQLite.
        let app = app_state(Box::new(UnavailableSecretStore));
        let profile = provider(&app, "Provider A");

        let message = set_provider_secret_in(&app, &profile.id, KEY).unwrap_err();
        assert_eq!(
            message,
            "secure credential storage error: the credential store is unavailable"
        );
        assert!(!message.contains(KEY), "leaked: {message}");

        let message = provider_secret_status_in(&app, &profile.id).unwrap_err();
        assert!(message.starts_with("secure credential storage error"), "{message}");
        assert!(!message.contains(KEY), "leaked: {message}");

        let message = test_target_in(&app, &profile.id).unwrap_err();
        assert!(message.starts_with("secure credential storage error"), "{message}");
        assert!(!message.contains(KEY), "leaked: {message}");
    }

    #[test]
    fn a_failed_secret_delete_leaves_the_profile_so_no_credential_is_stranded() {
        // The delete order exists for exactly this case: if the credential
        // cannot be removed, the profile row must survive, otherwise the
        // keyring entry would be unreachable forever (and the user would have
        // no way to retry).
        let app = app_state(Box::new(UnavailableSecretStore));
        let profile = provider(&app, "Provider A");

        let message = delete_provider_in(&app, &profile.id).unwrap_err();
        assert!(message.starts_with("secure credential storage error"), "{message}");

        assert!(
            ProviderRepository::new(&app.storage)
                .find(&profile.id)
                .unwrap()
                .is_some(),
            "the profile must survive a failed credential delete"
        );
    }

    #[test]
    fn deleting_a_provider_removes_the_credential_before_the_row() {
        let app = app_state(Box::new(InMemorySecretStore::new()));
        let profile = provider(&app, "Provider A");
        set_provider_secret_in(&app, &profile.id, KEY).unwrap();

        delete_provider_in(&app, &profile.id).unwrap();

        assert_eq!(
            app.secrets.get(&profile.id).unwrap(),
            None,
            "the keyring entry must not outlive the profile"
        );
        assert!(ProviderRepository::new(&app.storage)
            .find(&profile.id)
            .unwrap()
            .is_none());
        // Deleting the profile's row twice is reported as "not found":
        // the leftover credential is gone, so there is nothing to retry.
        assert_eq!(
            delete_provider_in(&app, &profile.id).unwrap_err(),
            format!("provider not found: {}", profile.id)
        );
    }

    #[test]
    fn the_connectivity_check_reads_its_target_without_exposing_the_key() {
        let app = app_state(Box::new(InMemorySecretStore::new()));
        let profile = provider(&app, "Provider A");
        set_provider_secret_in(&app, &profile.id, KEY).unwrap();

        let (base_url, api_key) = test_target_in(&app, &profile.id).unwrap();
        assert_eq!(base_url, "https://provider-a.example.com");
        // The key is handed to the HTTP client in memory only; the command's
        // result (`ProviderTestResult`) has no field that could carry it (spec
        // sections 12, 17).
        assert_eq!(api_key.as_deref(), Some(KEY));
    }
}
