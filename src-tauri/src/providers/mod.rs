//! Provider profile management (spec sections 5, 12, 19).
//!
//! A provider profile is the non-secret half of the configuration needed to
//! talk to an AI API: id, display name, base URL, default model, extra
//! environment variables, and - when the model is unknown to Claude Code's own
//! catalog - the model's real context window. The API key is *not* part of the
//! persisted profile (spec sections 5, 14, 17) - it lives in the OS keyring
//! under the same id (see [`crate::secrets`]) and is only joined to the profile
//! in [`ResolvedProvider`], in memory, at the moment a session is started or a
//! connection is tested.
//!
//! [`ProviderRepository`] is the only thing that reads or writes the
//! `providers` table. It never touches [`crate::secrets`]: keeping the
//! repository secret-free means a bug here cannot leak a credential, and it is
//! directly asserted by a test (`api_key_is_never_persisted_in_sqlite`).

pub mod connectivity;

use std::fmt;

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::persistence::{now_rfc3339, Storage, StorageError};

/// Convenience alias for provider operations.
pub type Result<T> = std::result::Result<T, ProviderError>;

/// Smallest value accepted for [`ProviderInput::max_context_tokens`].
///
/// The variable exists to correct Claude Code's *assumed* 200k window for a
/// model it does not know, so anything below this is a typo (a unit mix-up, a
/// stray `0`) rather than a real context window - the smallest shipped windows
/// are in the thousands. There is deliberately no upper bound: context windows
/// keep growing, and a too-large declared window is the user's own model
/// configuration to get right, whereas a silently ignored setting is not.
pub const MIN_MAX_CONTEXT_TOKENS: u32 = 1_000;

/// Errors produced by provider profile operations.
///
/// Every message is user-facing and secret-free (spec section 16). Note that
/// [`ProviderError::CredentialsInBaseUrl`] exists specifically to stop a
/// credential from being stored as provider metadata - see
/// [`ProviderInput::validate`].
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The persistence layer failed.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// The profile id has no row.
    #[error("provider not found: {0}")]
    NotFound(String),

    /// `name` was empty or whitespace only.
    #[error("provider name must not be empty")]
    EmptyName,

    /// `base_url` was empty or whitespace only.
    #[error("base URL must not be empty")]
    EmptyBaseUrl,

    /// `base_url` was not an absolute `http`/`https` URL.
    #[error("base URL must be an absolute http(s) URL, for example https://api.example.com")]
    InvalidBaseUrl,

    /// `base_url` embedded credentials in its authority.
    #[error(
        "base URL must not contain credentials (user:password@host) - put the API key in the API key field instead, because base URLs are stored as plaintext metadata"
    )]
    CredentialsInBaseUrl,

    /// `model` was empty or whitespace only.
    #[error("model must not be empty")]
    EmptyModel,

    /// An extra environment variable had an empty name.
    #[error("environment variable name must not be empty")]
    EmptyEnvironmentKey,

    /// An extra environment variable name contained `=` or NUL, which no
    /// platform can pass to a child process.
    #[error("environment variable name `{name}` must not contain `=` or NUL")]
    InvalidEnvironmentKey {
        /// The offending variable name (never a value).
        name: String,
    },

    /// The extra environment variables could not be encoded for storage.
    #[error("could not encode extra environment variables: {0}")]
    ExtraEnvironmentEncoding(String),

    /// `max_context_tokens` was set to a value below
    /// [`MIN_MAX_CONTEXT_TOKENS`] (this includes `0`), which no real model's
    /// context window is.
    ///
    /// Carries only the number the user typed - never a credential (spec
    /// section 17).
    #[error(
        "max context tokens must be at least {MIN_MAX_CONTEXT_TOKENS} (a model's real context window), got {value} - leave the field empty unless Claude Code reports the model as unknown to its catalog"
    )]
    MaxContextTokensTooSmall {
        /// The rejected value, as submitted.
        value: u32,
    },
}

/// A provider's additional environment variable (`(name, value)` pair).
///
/// Serialized to and from JSON in the `extra_env_json` column, and exposed to
/// the frontend as a two-element array.
pub type EnvironmentPair = (String, String);

/// A persisted provider profile (spec section 5) without any secret material.
///
/// Serialized `camelCase` for the React frontend. `created_at`/`updated_at`
/// live in the database but are intentionally not part of this type: nothing in
/// the UI needs them yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    /// Stable id; also the keyring account name for this provider's API key.
    pub id: String,
    /// Display name, for example "Free Provider A".
    pub name: String,
    /// Base URL of an Anthropic-compatible API, for example
    /// `https://provider-a.example.com`.
    pub base_url: String,
    /// Default model id for this provider.
    pub model: String,
    /// Extra environment variables layered into the session environment after
    /// the defaults (spec sections 5, 7).
    pub extra_env: Vec<EnvironmentPair>,
    /// The model's real context window in tokens, when the user declares one,
    /// because Claude Code's shipped model catalog does not know the id and
    /// would otherwise size auto-compact from an assumed 200k window. `None`
    /// (the default) leaves Claude Code's own fallback in charge. Persisted as
    /// the nullable `max_context_tokens` column.
    pub max_context_tokens: Option<u32>,
}

/// Caller-supplied profile payload for create/update.
///
/// Separate from [`ProviderProfile`] because the id is generated by the
/// repository and the API key is handled by [`crate::secrets`], never here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    /// Display name.
    pub name: String,
    /// Anthropic-compatible base URL.
    pub base_url: String,
    /// Default model id.
    pub model: String,
    /// Extra environment variables (may be empty).
    #[serde(default)]
    pub extra_env: Vec<EnvironmentPair>,
    /// The model's real context window in tokens, when the user declares one
    /// (spec section 7). `None` clears it back to the database's `NULL`, which
    /// leaves Claude Code's own 200k fallback in charge. Absent in the JSON
    /// payload counts as `None` (`#[serde(default)]`), so an older frontend
    /// keeps working.
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
}

impl ProviderInput {
    /// Validate the payload before it is persisted (spec section 16).
    ///
    /// Rejects empty required fields, non-absolute base URLs, base URLs that
    /// carry credentials - the latter would otherwise be written to the SQLite
    /// file as plaintext, directly violating spec sections 5 and 17 - and a
    /// `max_context_tokens` below [`MIN_MAX_CONTEXT_TOKENS`], because a wrong
    /// declared window silently mis-sizes auto-compact instead of failing
    /// loudly.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(ProviderError::EmptyName);
        }
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            return Err(ProviderError::EmptyBaseUrl);
        }
        if !is_absolute_http_url(base_url) {
            return Err(ProviderError::InvalidBaseUrl);
        }
        if base_url_has_credentials(base_url) {
            return Err(ProviderError::CredentialsInBaseUrl);
        }
        if self.model.trim().is_empty() {
            return Err(ProviderError::EmptyModel);
        }
        if let Some(max_context_tokens) = self.max_context_tokens {
            // Covers `0`, which `Option` cannot distinguish from "declared".
            if max_context_tokens < MIN_MAX_CONTEXT_TOKENS {
                return Err(ProviderError::MaxContextTokensTooSmall {
                    value: max_context_tokens,
                });
            }
        }
        for (key, _value) in &self.extra_env {
            if key.trim().is_empty() {
                return Err(ProviderError::EmptyEnvironmentKey);
            }
            if key.contains('=') || key.contains('\0') {
                return Err(ProviderError::InvalidEnvironmentKey { name: key.clone() });
            }
        }
        Ok(())
    }
}

/// A provider profile joined with its API key, ready to build a process
/// environment (spec sections 5, 7).
///
/// The key arrives from the OS keyring at the last possible moment and is
/// deliberately **not** `Serialize`: it can never be sent to the frontend or
/// persisted. `Debug` is hand-written to redact it, so an accidental log line
/// cannot leak the credential (spec section 17).
#[derive(Clone)]
pub struct ResolvedProvider {
    /// Profile id (keyring account).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Base URL of the API.
    pub base_url: String,
    /// Model id.
    pub model: String,
    /// Extra environment variables configured for this provider.
    pub extra_env: Vec<EnvironmentPair>,
    /// The model's real context window when the profile declares one, used to
    /// set `CLAUDE_CODE_MAX_CONTEXT_TOKENS` for a session (spec section 7).
    /// `None` means "let Claude Code fall back to its own assumption".
    pub max_context_tokens: Option<u32>,
    /// API key from the keyring. `None` means "no key stored", which is valid
    /// for endpoints that do not require one.
    pub(crate) api_key: Option<String>,
}

impl ResolvedProvider {
    /// Join a stored profile with its API key.
    pub fn new(profile: ProviderProfile, api_key: Option<String>) -> Self {
        Self {
            id: profile.id,
            name: profile.name,
            base_url: profile.base_url,
            model: profile.model,
            extra_env: profile.extra_env,
            max_context_tokens: profile.max_context_tokens,
            api_key,
        }
    }

    /// The API key, if one is stored. Callers must not log or persist it.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

impl fmt::Debug for ResolvedProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProvider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("extra_env", &self.extra_env)
            .field("max_context_tokens", &self.max_context_tokens)
            .field(
                "api_key",
                &if self.api_key.is_some() {
                    "<redacted>"
                } else {
                    "<not set>"
                },
            )
            .finish()
    }
}

/// CRUD over the `providers` table (spec sections 5, 14).
///
/// Borrows the shared [`Storage`] rather than owning a connection, so the
/// application keeps a single SQLite connection behind one mutex.
pub struct ProviderRepository<'storage> {
    storage: &'storage Storage,
}

impl<'storage> ProviderRepository<'storage> {
    /// Wrap the application database.
    pub fn new(storage: &'storage Storage) -> Self {
        Self { storage }
    }

    /// Columns selected for every profile read. Listed explicitly so a schema
    /// change cannot silently start feeding new columns to the frontend.
    const COLUMNS: &'static str =
        "id, name, base_url, model, extra_env_json, max_context_tokens";

    /// Insert a new profile and return it. The id is generated here.
    pub fn create(&self, input: &ProviderInput) -> Result<ProviderProfile> {
        input.validate()?;

        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let extra_env_json = encode_extra_env(&input.extra_env)?;

        self.storage.with_conn(|connection| {
            connection.execute(
                "INSERT INTO providers (id, name, base_url, model, extra_env_json, max_context_tokens, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id,
                    input.name.trim(),
                    input.base_url.trim(),
                    input.model.trim(),
                    extra_env_json,
                    input.max_context_tokens.map(i64::from),
                    now
                ],
            )?;
            Ok(())
        })?;

        self.get(&id)
    }

    /// Load one profile, failing when the id is unknown.
    pub fn get(&self, id: &str) -> Result<ProviderProfile> {
        self.find(id)?
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))
    }

    /// Load one profile, returning `None` when the id is unknown.
    pub fn find(&self, id: &str) -> Result<Option<ProviderProfile>> {
        let profile = self.storage.with_conn(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {} FROM providers WHERE id = ?1",
                Self::COLUMNS
            ))?;
            let mut rows = statement.query(params![id])?;
            match rows.next()? {
                Some(row) => Ok(Some(row_to_profile(row)?)),
                None => Ok(None),
            }
        })?;
        Ok(profile)
    }

    /// List all profiles, ordered for display (name, then creation order).
    pub fn list(&self) -> Result<Vec<ProviderProfile>> {
        let profiles = self.storage.with_conn(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {} FROM providers ORDER BY name COLLATE NOCASE ASC, created_at ASC",
                Self::COLUMNS
            ))?;
            let profiles = statement
                .query_map([], row_to_profile)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(profiles)
        })?;
        Ok(profiles)
    }

    /// Replace a profile's editable fields. `created_at` is preserved and
    /// `updated_at` is refreshed. The stored API key is untouched (it is not
    /// this layer's concern - see [`crate::secrets`]).
    ///
    /// `max_context_tokens` is written on every update, so an input without one
    /// clears the column back to `NULL` rather than leaving a stale window
    /// behind (the frontend's "clear the field" case).
    pub fn update(&self, id: &str, input: &ProviderInput) -> Result<ProviderProfile> {
        input.validate()?;

        let now = now_rfc3339();
        let extra_env_json = encode_extra_env(&input.extra_env)?;

        let updated = self.storage.with_conn(|connection| {
            let updated = connection.execute(
                "UPDATE providers
                    SET name = ?2, base_url = ?3, model = ?4, extra_env_json = ?5,
                        max_context_tokens = ?6, updated_at = ?7
                  WHERE id = ?1",
                params![
                    id,
                    input.name.trim(),
                    input.base_url.trim(),
                    input.model.trim(),
                    extra_env_json,
                    input.max_context_tokens.map(i64::from),
                    now
                ],
            )?;
            Ok(updated)
        })?;

        // No matching row: report it the same way `get` does. The zero-row
        // check stays outside the storage closure so the error is a
        // `ProviderError::NotFound` rather than a wrapped `StorageError`.
        if updated == 0 {
            return Err(ProviderError::NotFound(id.to_string()));
        }

        self.get(id)
    }

    /// Delete a profile. Returns `false` when the id was already gone.
    ///
    /// The keyring entry is *not* deleted here: the repository never touches
    /// secrets. [`crate::commands::providers::delete_provider`] removes both.
    /// Workspace rows that reference the deleted provider are intentionally
    /// left in place (spec section 9: removing configuration must not cascade
    /// into other user data).
    pub fn delete(&self, id: &str) -> Result<bool> {
        let deleted = self.storage.with_conn(|connection| {
            let deleted = connection.execute("DELETE FROM providers WHERE id = ?1", params![id])?;
            Ok(deleted > 0)
        })?;
        Ok(deleted)
    }
}

/// Map a `providers` row (in [`ProviderRepository::COLUMNS`] order) to a
/// profile.
fn row_to_profile(row: &Row<'_>) -> rusqlite::Result<ProviderProfile> {
    let id: String = row.get("id")?;
    let extra_env_json: String = row.get("extra_env_json")?;
    let raw_max_context_tokens: Option<i64> = row.get("max_context_tokens")?;
    // Both decoders borrow `id`, so they run before it is moved into the struct.
    let max_context_tokens = decode_max_context_tokens(&id, raw_max_context_tokens);
    Ok(ProviderProfile {
        // Decoding is lossy by design: one malformed row must not make the
        // whole provider list unusable. The raw JSON stays in the database.
        extra_env: decode_extra_env(&id, &extra_env_json),
        id,
        name: row.get("name")?,
        base_url: row.get("base_url")?,
        model: row.get("model")?,
        max_context_tokens,
    })
}

/// Decode the nullable `max_context_tokens` column.
///
/// Lossy for the same reason [`decode_extra_env`] is: the column is a plain
/// `INTEGER`, so a hand-edited or downgraded-then-upgraded database can hold a
/// value that is not a valid token count. Such a row is reported as "no window
/// declared" - Claude Code's own fallback, and never a bogus
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS` - with a log line, instead of failing every
/// read of the provider list.
fn decode_max_context_tokens(provider_id: &str, raw: Option<i64>) -> Option<u32> {
    match raw {
        None => None,
        Some(value) => match u32::try_from(value) {
            Ok(value) => Some(value),
            Err(_) => {
                log::warn!(
                    "provider {provider_id}: ignoring out-of-range max_context_tokens ({value})"
                );
                None
            }
        },
    }
}

/// Encode extra environment variables as the JSON text stored in
/// `extra_env_json` (for example `[["ANTHROPIC_AUTH_TOKEN","token"]]`).
fn encode_extra_env(extra_env: &[EnvironmentPair]) -> Result<String> {
    serde_json::to_string(extra_env)
        .map_err(|error| ProviderError::ExtraEnvironmentEncoding(error.to_string()))
}

/// Decode `extra_env_json`, falling back to an empty list for a row written by
/// an older/faulty version instead of failing the read.
fn decode_extra_env(provider_id: &str, raw: &str) -> Vec<EnvironmentPair> {
    match serde_json::from_str::<Vec<EnvironmentPair>>(raw) {
        Ok(extra_env) => extra_env,
        Err(error) => {
            log::warn!("provider {provider_id}: ignoring malformed extra_env_json ({error})");
            Vec::new()
        }
    }
}

/// Whether `value` is an absolute `http`/`https` URL with a non-empty host.
///
/// Deliberately a conservative hand-rolled check instead of pulling in a URL
/// crate for Milestone 2; it only needs to reject obvious mistakes before the
/// value reaches the database and the HTTP client.
fn is_absolute_http_url(value: &str) -> bool {
    let rest = match value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    {
        Some(rest) => rest,
        None => return false,
    };
    if rest.is_empty() || rest.starts_with('/') {
        return false;
    }
    !rest.chars().any(char::is_whitespace)
}

/// Whether the URL's authority contains `user[:password]@`, which would store
/// a credential as plaintext provider metadata (spec sections 5, 17).
fn base_url_has_credentials(value: &str) -> bool {
    let authority = value
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or(value)
        .split(|character| character == '/' || character == '?' || character == '#')
        .next()
        .unwrap_or_default();
    authority.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::TempDb;
    use crate::secrets::memory::InMemorySecretStore;
    use crate::secrets::SecretStore;

    fn input(name: &str) -> ProviderInput {
        ProviderInput {
            name: name.to_string(),
            base_url: "https://provider-a.example.com".to_string(),
            model: "model-a".to_string(),
            extra_env: Vec::new(),
            max_context_tokens: None,
        }
    }

    #[test]
    fn provider_round_trip_create_read_list_update_delete() {
        let database = TempDb::new("provider-crud");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);

        assert!(repository.list().unwrap().is_empty());

        let created = repository.create(&input("Provider A")).unwrap();
        assert!(!created.id.is_empty());
        assert_eq!(created.name, "Provider A");
        assert_eq!(created.base_url, "https://provider-a.example.com");
        assert_eq!(created.model, "model-a");
        assert!(created.extra_env.is_empty());

        // Read back, including a fresh repository over the same storage.
        let loaded = ProviderRepository::new(&storage).get(&created.id).unwrap();
        assert_eq!(loaded, created);

        // Whitespace around input is trimmed before persisting.
        let trimmed = repository
            .create(&ProviderInput {
                name: "  Provider B  ".to_string(),
                base_url: "  https://provider-b.example.com  ".to_string(),
                model: "  model-b  ".to_string(),
                extra_env: vec![("ANTHROPIC_AUTH_TOKEN".to_string(), "token-b".to_string())],
                max_context_tokens: Some(200_000),
            })
            .unwrap();
        assert_eq!(trimmed.name, "Provider B");
        assert_eq!(trimmed.base_url, "https://provider-b.example.com");
        assert_eq!(trimmed.model, "model-b");
        // The declared window round-trips through the integer column.
        assert_eq!(trimmed.max_context_tokens, Some(200_000));

        // List is stable and ordered by name.
        let listed = repository.list().unwrap();
        assert_eq!(
            listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["Provider A", "Provider B"]
        );
        assert_eq!(
            listed
                .iter()
                .find(|p| p.id == trimmed.id)
                .unwrap()
                .extra_env,
            vec![("ANTHROPIC_AUTH_TOKEN".to_string(), "token-b".to_string())]
        );

        // Update replaces editable fields and preserves the id.
        let updated = repository
            .update(
                &created.id,
                &ProviderInput {
                    name: "Provider A (edited)".to_string(),
                    base_url: "https://provider-a.example.com/v1".to_string(),
                    model: "model-a-2".to_string(),
                    extra_env: vec![("ANTHROPIC_CUSTOM_HEADERS".to_string(), "x=1".to_string())],
                    max_context_tokens: Some(1_000_000),
                },
            )
            .unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "Provider A (edited)");
        assert_eq!(updated.base_url, "https://provider-a.example.com/v1");
        assert_eq!(updated.max_context_tokens, Some(1_000_000));
        assert_eq!(
            repository.get(&created.id).unwrap().extra_env,
            vec![("ANTHROPIC_CUSTOM_HEADERS".to_string(), "x=1".to_string())]
        );

        // Delete removes exactly one row; a second delete reports "already gone".
        assert!(repository.delete(&created.id).unwrap());
        assert!(!repository.delete(&created.id).unwrap());
        assert!(matches!(
            repository.get(&created.id),
            Err(ProviderError::NotFound(_))
        ));
        assert_eq!(repository.list().unwrap().len(), 1);
    }

    #[test]
    fn updating_an_unknown_provider_reports_not_found() {
        let database = TempDb::new("provider-update-missing");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);
        assert!(matches!(
            repository.update("does-not-exist", &input("Provider X")),
            Err(ProviderError::NotFound(_))
        ));
    }

    #[test]
    fn extra_env_is_stored_as_json_text() {
        let database = TempDb::new("provider-extra-env");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);

        let created = repository
            .create(&ProviderInput {
                extra_env: vec![
                    ("ANTHROPIC_AUTH_TOKEN".to_string(), "token-a".to_string()),
                    ("ENABLE_TOOL_SEARCH".to_string(), "true".to_string()),
                ],
                ..input("Provider A")
            })
            .unwrap();

        let raw: String = storage
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT extra_env_json FROM providers WHERE id = ?1",
                    params![created.id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            raw,
            r#"[["ANTHROPIC_AUTH_TOKEN","token-a"],["ENABLE_TOOL_SEARCH","true"]]"#
        );

        // And it survives the round trip in order.
        assert_eq!(repository.get(&created.id).unwrap().extra_env, created.extra_env);
    }

    #[test]
    fn malformed_extra_env_json_does_not_break_reads() {
        let database = TempDb::new("provider-bad-json");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);
        let created = repository.create(&input("Provider A")).unwrap();

        storage
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE providers SET extra_env_json = ?2 WHERE id = ?1",
                    params![created.id, "{not json"],
                )?;
                Ok(())
            })
            .unwrap();

        assert!(repository.get(&created.id).unwrap().extra_env.is_empty());
        assert_eq!(repository.list().unwrap().len(), 1);
    }

    // --- the declared context window (FIX 2) --------------------------------

    #[test]
    fn a_declared_context_window_round_trips_and_can_be_cleared_back_to_null() {
        let database = TempDb::new("provider-context-window");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);

        let created = repository
            .create(&ProviderInput {
                max_context_tokens: Some(200_000),
                ..input("Provider A")
            })
            .unwrap();
        assert_eq!(created.max_context_tokens, Some(200_000));

        let stored = |id: &str| -> Option<i64> {
            storage
                .with_conn(|connection| {
                    Ok(connection.query_row(
                        "SELECT max_context_tokens FROM providers WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )?)
                })
                .unwrap()
        };
        assert_eq!(stored(&created.id), Some(200_000));

        // An update without a window clears the column. It must become NULL -
        // not a stale value and not `0`, which would silently declare a
        // zero-token window to Claude Code.
        let updated = repository.update(&created.id, &input("Provider A")).unwrap();
        assert_eq!(updated.max_context_tokens, None);
        assert_eq!(stored(&created.id), None, "the column must be NULL, not 0");
        assert_eq!(repository.get(&created.id).unwrap().max_context_tokens, None);
    }

    #[test]
    fn the_context_window_is_camel_case_on_the_wire_and_optional() {
        // `maxContextTokens` is the frontend's field name ...
        let with_field: ProviderInput = serde_json::from_str(
            r#"{"name":"Provider A","baseUrl":"https://provider-a.example.com","model":"model-a","maxContextTokens":1000000}"#,
        )
        .unwrap();
        assert_eq!(with_field.max_context_tokens, Some(1_000_000));

        // ... and a payload that predates the field (or carries an explicit
        // null) still deserializes, which is what keeps an older frontend
        // working against this build.
        let without_field: ProviderInput = serde_json::from_str(
            r#"{"name":"Provider A","baseUrl":"https://provider-a.example.com","model":"model-a"}"#,
        )
        .unwrap();
        assert_eq!(without_field.max_context_tokens, None);
        let explicit_null: ProviderInput = serde_json::from_str(
            r#"{"name":"Provider A","baseUrl":"https://provider-a.example.com","model":"model-a","maxContextTokens":null}"#,
        )
        .unwrap();
        assert_eq!(explicit_null.max_context_tokens, None);

        // The profile the frontend reads names the field the same way.
        let profile = ProviderProfile {
            id: "provider-a".to_string(),
            name: "Provider A".to_string(),
            base_url: "https://provider-a.example.com".to_string(),
            model: "model-a".to_string(),
            extra_env: Vec::new(),
            max_context_tokens: Some(200_000),
        };
        let json = serde_json::to_value(&profile).unwrap();
        assert_eq!(json["maxContextTokens"], serde_json::json!(200_000));
        let undeclared = serde_json::to_value(ProviderProfile {
            max_context_tokens: None,
            ..profile
        })
        .unwrap();
        assert_eq!(undeclared["maxContextTokens"], serde_json::Value::Null);
    }

    #[test]
    fn a_context_window_below_the_minimum_is_rejected_with_a_clear_message() {
        let database = TempDb::new("provider-context-window-min");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);

        for value in [0, 1, MIN_MAX_CONTEXT_TOKENS - 1] {
            let error = repository
                .create(&ProviderInput {
                    max_context_tokens: Some(value),
                    ..input("Provider A")
                })
                .expect_err("a context window below the minimum must be refused");

            assert!(
                matches!(
                    error,
                    ProviderError::MaxContextTokensTooSmall { value: rejected }
                        if rejected == value
                ),
                "unexpected error for {value}: {error:?}"
            );
            let message = error.to_string();
            assert!(
                message.contains("max context tokens"),
                "the message must name the setting: {message}"
            );
            assert!(
                message.contains(&MIN_MAX_CONTEXT_TOKENS.to_string()),
                "the message must say what is acceptable: {message}"
            );
            assert!(
                message.contains(&value.to_string()),
                "the message must echo the rejected value: {message}"
            );
            // Only the number the user typed is echoed - never a credential.
            assert!(!message.contains("sk-"), "leaked: {message}");
        }

        assert!(
            repository.list().unwrap().is_empty(),
            "nothing may be persisted when validation fails"
        );

        // The boundary itself is accepted: the rule rejects typos, not real
        // context windows.
        let accepted = repository
            .create(&ProviderInput {
                max_context_tokens: Some(MIN_MAX_CONTEXT_TOKENS),
                ..input("Provider A")
            })
            .unwrap();
        assert_eq!(accepted.max_context_tokens, Some(MIN_MAX_CONTEXT_TOKENS));
    }

    #[test]
    fn an_impossible_context_window_in_a_row_reads_as_undeclared() {
        // The column is a plain `INTEGER`, so a hand-edited (or
        // downgraded-then-upgraded) database can hold something that is not a
        // token count. One bad row must not make the provider list unreadable,
        // and it must never produce a bogus declaration for a session.
        let database = TempDb::new("provider-bad-context-window");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);
        let created = repository.create(&input("Provider A")).unwrap();

        for impossible in [-1_i64, i64::MIN, i64::from(u32::MAX) + 1] {
            storage
                .with_conn(|connection| {
                    connection.execute(
                        "UPDATE providers SET max_context_tokens = ?2 WHERE id = ?1",
                        params![created.id, impossible],
                    )?;
                    Ok(())
                })
                .unwrap();

            assert_eq!(
                repository.get(&created.id).unwrap().max_context_tokens,
                None,
                "a stored {impossible} is not a window"
            );
            assert_eq!(repository.list().unwrap().len(), 1);
        }
    }

    #[test]
    fn validation_rejects_incomplete_or_unsafe_profiles() {
        let database = TempDb::new("provider-validation");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);

        let cases: Vec<(ProviderInput, &str)> = vec![
            (
                ProviderInput {
                    name: "   ".to_string(),
                    ..input("ignored")
                },
                "empty name",
            ),
            (
                ProviderInput {
                    base_url: String::new(),
                    ..input("Provider A")
                },
                "empty base url",
            ),
            (
                ProviderInput {
                    base_url: "provider-a.example.com".to_string(),
                    ..input("Provider A")
                },
                "missing scheme",
            ),
            (
                ProviderInput {
                    base_url: "ftp://provider-a.example.com".to_string(),
                    ..input("Provider A")
                },
                "wrong scheme",
            ),
            (
                ProviderInput {
                    base_url: "https://user:sk-secret@provider-a.example.com".to_string(),
                    ..input("Provider A")
                },
                "credentials in url",
            ),
            (
                ProviderInput {
                    model: "  ".to_string(),
                    ..input("Provider A")
                },
                "empty model",
            ),
            (
                ProviderInput {
                    extra_env: vec![("".to_string(), "value".to_string())],
                    ..input("Provider A")
                },
                "empty env key",
            ),
            (
                ProviderInput {
                    extra_env: vec![("BAD=KEY".to_string(), "value".to_string())],
                    ..input("Provider A")
                },
                "env key with equals",
            ),
        ];

        for (case, label) in cases {
            let error = repository
                .create(&case)
                .expect_err(&format!("{label} must be rejected"));
            assert!(
                !error.to_string().contains("sk-secret"),
                "{label}: error text leaked the credential: {error}"
            );
        }

        assert!(repository.list().unwrap().is_empty());
    }

    #[test]
    fn api_key_is_never_persisted_in_sqlite() {
        // The cross-layer guarantee from spec sections 5, 14 and 17: a secret
        // goes into the secret store, the database only ever sees metadata.
        let database = TempDb::new("provider-secret-isolation");
        let storage = database.storage();
        let repository = ProviderRepository::new(&storage);
        let secrets = InMemorySecretStore::new();

        let secret = "sk-test-DO-NOT-PERSIST-9c41fb7e";
        let created = repository.create(&input("Provider A")).unwrap();
        secrets.set(&created.id, secret).unwrap();

        // 1. No column in the providers table carries the key. (`COALESCE`
        //    because `||` with a NULL operand yields NULL, which would make the
        //    query itself fail rather than compare anything.)
        let concatenated: String = storage
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT id || name || base_url || model || extra_env_json
                            || COALESCE(max_context_tokens, '') || created_at || updated_at
                       FROM providers",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(!concatenated.contains(secret));

        // 2. No column anywhere in the file carries the key.
        let file = String::from_utf8_lossy(&database.raw_bytes()).into_owned();
        assert!(
            !file.contains(secret),
            "the SQLite file must never contain an API key"
        );

        // 3. The keyring is where it actually lives, and it round-trips.
        assert_eq!(
            secrets.get(&created.id).unwrap(),
            Some(secret.to_string())
        );
    }

    #[test]
    fn resolved_provider_redacts_the_api_key() {
        let profile = ProviderProfile {
            id: "provider-a".to_string(),
            name: "Provider A".to_string(),
            base_url: "https://provider-a.example.com".to_string(),
            model: "model-a".to_string(),
            extra_env: Vec::new(),
            max_context_tokens: Some(1_000_000),
        };
        let resolved = ResolvedProvider::new(profile, Some("sk-test-key".to_string()));

        let rendered = format!("{resolved:?}");
        assert!(!rendered.contains("sk-test-key"), "leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
        assert_eq!(resolved.api_key(), Some("sk-test-key"));
    }
}
