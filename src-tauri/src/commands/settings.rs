//! Settings command surface (spec sections 12, 14, 16).
//!
//! The Settings tab is backed by the `ui_preferences` table: non-secret
//! terminal, session and workspace preferences stored as opaque strings. Three
//! rules apply here:
//!
//! - Keys are validated against [`ALLOWED_KEYS`], so a typo cannot silently
//!   accumulate junk rows in the database.
//! - An empty value *deletes* the row, so "unset" and "set to empty" stay
//!   indistinguishable and the frontend default applies again.
//! - No value here is a credential. Secrets live only in the OS keyring (spec
//!   section 17), and this module never touches [`crate::secrets`].
//!
//! The module also hosts the read-only "About / Storage" information for the
//! same tab.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::agents::claude_code::ClaudeCodeAdapter;
use crate::agents::AgentAdapter;
use crate::commands::{error_message, lock_state, AppState};
use crate::persistence::Storage;

/// Preference key: which palette the UI paints with (`dark`/`light`/`system`).
///
/// The value is deliberately *not* validated here: like every other preference
/// it is an opaque string whose parsing and default live in the frontend (spec
/// section 14). A value this build does not recognise therefore falls back to
/// the frontend default instead of failing the write.
pub const KEY_APPEARANCE_THEME: &str = "appearance.theme";
/// Preference key: terminal font size in px (integer 8-24).
pub const KEY_TERMINAL_FONT_SIZE: &str = "terminal.fontSize";
/// Preference key: lines of scrollback the terminal keeps (1000-50000).
pub const KEY_TERMINAL_SCROLLBACK: &str = "terminal.scrollback";
/// Preference key: whether the terminal cursor blinks ("true"/"false").
pub const KEY_TERMINAL_CURSOR_BLINK: &str = "terminal.cursorBlink";
/// Preference key: copy the selection as soon as it is made ("true"/"false").
pub const KEY_TERMINAL_COPY_ON_SELECT: &str = "terminal.copyOnSelect";
/// Preference key: restore the previously open tab set at launch.
pub const KEY_SESSIONS_RESTORE_TABS: &str = "sessions.restoreTabs";
/// Preference key: confirm before closing a tab whose session is running.
pub const KEY_SESSIONS_CONFIRM_CLOSE_RUNNING: &str = "sessions.confirmCloseRunning";
/// Preference key: provider preselected in the New Workspace dialog.
pub const KEY_WORKSPACES_DEFAULT_PROVIDER: &str = "workspaces.defaultProviderId";

/// Every key the Settings tab is allowed to write.
pub const ALLOWED_KEYS: &[&str] = &[
    KEY_APPEARANCE_THEME,
    KEY_TERMINAL_FONT_SIZE,
    KEY_TERMINAL_SCROLLBACK,
    KEY_TERMINAL_CURSOR_BLINK,
    KEY_TERMINAL_COPY_ON_SELECT,
    KEY_SESSIONS_RESTORE_TABS,
    KEY_SESSIONS_CONFIRM_CLOSE_RUNNING,
    KEY_WORKSPACES_DEFAULT_PROVIDER,
];

/// Paths resolved once during startup.
///
/// Kept in managed state so the "About / Storage" section reports the real
/// files the application is using instead of re-deriving them from platform
/// conventions (spec section 18).
pub struct AppPaths {
    /// Platform app-data directory holding the database and session configs.
    pub data_directory: PathBuf,
    /// The application database file.
    pub database_path: PathBuf,
}

/// Read-only application information for the Settings tab (spec section 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Application version from the bundle metadata.
    pub version: String,
    /// Directory holding the database and per-session configuration.
    pub data_directory: String,
    /// The SQLite database file.
    pub database_path: String,
    /// Database file size in bytes; `None` when the file does not exist yet.
    pub database_size_bytes: Option<u64>,
    /// Resolved Claude Code executable, or `None` when it is not installed.
    pub claude_code_path: Option<String>,
    /// Schema version the database is currently migrated to.
    pub schema_version: i64,
}

/// Validate a preference key against [`ALLOWED_KEYS`].
///
/// Returns the static key so callers cannot accidentally use an unvalidated
/// `&str` against the database.
pub fn validate_key(key: &str) -> Result<&'static str, String> {
    ALLOWED_KEYS
        .iter()
        .copied()
        .find(|allowed| *allowed == key)
        .ok_or_else(|| format!("unknown setting: {key}"))
}

/// Read every stored preference.
pub fn list_preferences(storage: &Storage) -> Result<HashMap<String, String>, String> {
    storage.list_ui_preferences().map_err(error_message)
}

/// Store a preference, or delete it when `value` is empty (reset to default).
pub fn write_preference(storage: &Storage, key: &str, value: &str) -> Result<(), String> {
    let key = validate_key(key)?;
    if value.is_empty() {
        storage.delete_ui_preference(key).map_err(error_message)
    } else {
        storage.set_ui_preference(key, value).map_err(error_message)
    }
}

/// Assemble the About/Storage view.
///
/// Split from the command so it is testable without a Tauri application: the
/// only inputs are the storage, the resolved paths and the version string.
pub fn build_app_info(
    storage: &Storage,
    paths: &AppPaths,
    version: &str,
) -> Result<AppInfo, String> {
    let database_size_bytes = std::fs::metadata(&paths.database_path)
        .ok()
        .map(|metadata| metadata.len());
    let schema_version = storage.schema_version().map_err(error_message)?;
    let claude_code_path = ClaudeCodeAdapter::new()
        .executable_path()
        .map(|path| path.display().to_string());

    Ok(AppInfo {
        version: version.to_string(),
        data_directory: paths.data_directory.display().to_string(),
        database_path: paths.database_path.display().to_string(),
        database_size_bytes,
        claude_code_path,
        schema_version,
    })
}

/// Every stored preference (the Settings tab loads this once at startup).
#[tauri::command]
pub fn list_ui_preferences(
    state: State<'_, Mutex<AppState>>,
) -> Result<HashMap<String, String>, String> {
    let application = lock_state(&state)?;
    list_preferences(&application.storage)
}

/// Store one preference. An empty `value` resets it to its default.
#[tauri::command]
pub fn set_ui_preference(
    state: State<'_, Mutex<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    let application = lock_state(&state)?;
    write_preference(&application.storage, &key, &value)
}

/// Read-only application information for the Settings tab.
#[tauri::command]
pub fn app_info(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    paths: State<'_, AppPaths>,
) -> Result<AppInfo, String> {
    let version = app.package_info().version.to_string();
    let application = lock_state(&state)?;
    build_app_info(&application.storage, &paths, &version)
}

/// Open the application data directory in the OS file manager.
#[tauri::command]
pub fn open_data_directory(app: AppHandle, paths: State<'_, AppPaths>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let directory = paths.data_directory.display().to_string();
    app.opener()
        .open_path(directory.clone(), None::<&str>)
        .map_err(|error| format!("could not open {directory}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_directory(label: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-workspace-settings-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create the temporary directory");
        path
    }

    fn paths_for(directory: &PathBuf) -> AppPaths {
        AppPaths {
            data_directory: directory.clone(),
            database_path: directory.join("ai-workspace.sqlite3"),
        }
    }

    #[test]
    fn every_documented_key_is_allowed_and_unknown_keys_are_rejected() {
        for key in ALLOWED_KEYS {
            assert_eq!(validate_key(key).expect("allowlisted"), *key);
        }
        for key in [
            "terminal.fontsize",
            "nope",
            "",
            "terminal.fontSize ",
            "appearance.themes",
            "Appearance.theme",
        ] {
            let error = validate_key(key).expect_err("must be rejected");
            assert!(error.contains("unknown setting"), "{error}");
        }
    }

    /// The theme key stores an opaque string.
    ///
    /// Parsing lives in the frontend (spec section 14), so a value this build
    /// does not recognise must still be storable rather than rejected at the
    /// database boundary - that is what lets a newer build's value survive a
    /// downgrade instead of erroring the user's Settings tab.
    #[test]
    fn the_theme_value_is_opaque_and_not_validated() {
        let storage = Storage::open_in_memory().expect("in-memory database");

        write_preference(&storage, KEY_APPEARANCE_THEME, "light").expect("store");
        assert_eq!(
            storage.get_ui_preference(KEY_APPEARANCE_THEME).expect("read"),
            Some("light".to_string())
        );

        write_preference(&storage, KEY_APPEARANCE_THEME, "solarized-midnight")
            .expect("an unrecognised value is still storable");
        assert_eq!(
            storage.get_ui_preference(KEY_APPEARANCE_THEME).expect("read"),
            Some("solarized-midnight".to_string())
        );
    }

    #[test]
    fn preferences_round_trip_and_empty_values_delete() {
        let storage = Storage::open_in_memory().expect("in-memory database");

        write_preference(&storage, KEY_TERMINAL_FONT_SIZE, "14").expect("store");
        assert_eq!(
            storage.get_ui_preference(KEY_TERMINAL_FONT_SIZE).expect("read"),
            Some("14".to_string())
        );

        write_preference(&storage, KEY_TERMINAL_FONT_SIZE, "16").expect("overwrite");
        assert_eq!(
            storage.get_ui_preference(KEY_TERMINAL_FONT_SIZE).expect("read"),
            Some("16".to_string())
        );

        write_preference(&storage, KEY_TERMINAL_FONT_SIZE, "").expect("reset");
        assert_eq!(
            storage.get_ui_preference(KEY_TERMINAL_FONT_SIZE).expect("read"),
            None
        );
    }

    #[test]
    fn writes_reject_unknown_keys_and_store_nothing() {
        let storage = Storage::open_in_memory().expect("in-memory database");
        let error = write_preference(&storage, "terminal.lineHeight", "2").expect_err("rejected");
        assert!(error.contains("unknown setting"), "{error}");
        assert!(list_preferences(&storage).expect("list").is_empty());
    }

    #[test]
    fn listing_returns_every_stored_preference() {
        let storage = Storage::open_in_memory().expect("in-memory database");
        write_preference(&storage, KEY_TERMINAL_FONT_SIZE, "13").expect("store");
        write_preference(&storage, KEY_SESSIONS_RESTORE_TABS, "true").expect("store");

        let listed = list_preferences(&storage).expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed.get(KEY_TERMINAL_FONT_SIZE).map(String::as_str), Some("13"));
        assert_eq!(
            listed.get(KEY_SESSIONS_RESTORE_TABS).map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn preferences_survive_reopening_the_database() {
        let directory = temp_directory("reopen");
        let paths = paths_for(&directory);

        {
            let storage = Storage::open(&paths.database_path).expect("open database");
            write_preference(&storage, KEY_TERMINAL_SCROLLBACK, "20000").expect("store");
        }

        let storage = Storage::open(&paths.database_path).expect("reopen database");
        assert_eq!(
            storage.get_ui_preference(KEY_TERMINAL_SCROLLBACK).expect("read"),
            Some("20000".to_string())
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn app_info_reports_the_real_paths_and_schema_version() {
        let directory = temp_directory("info");
        let paths = paths_for(&directory);
        let storage = Storage::open(&paths.database_path).expect("open database");

        let info = build_app_info(&storage, &paths, "0.1.0").expect("build info");

        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.database_path, paths.database_path.display().to_string());
        assert_eq!(info.data_directory, directory.display().to_string());
        assert!(info.schema_version >= 1, "migrations must have run");
        assert!(
            info.database_size_bytes.unwrap_or_default() > 0,
            "the database file exists after opening it"
        );
        if let Some(path) = info.claude_code_path {
            assert!(path.to_lowercase().contains("claude"), "{path}");
        }

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn app_info_reports_no_size_when_the_database_is_missing() {
        let directory = temp_directory("missing");
        let paths = paths_for(&directory);
        let storage = Storage::open_in_memory().expect("in-memory database");

        let info = build_app_info(&storage, &paths, "0.1.0").expect("build info");
        assert_eq!(info.database_size_bytes, None);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn app_info_is_serialized_camel_case_for_the_frontend() {
        let info = AppInfo {
            version: "0.1.0".to_string(),
            data_directory: "C:/data".to_string(),
            database_path: "C:/data/ai-workspace.sqlite3".to_string(),
            database_size_bytes: Some(4096),
            claude_code_path: Some("C:/bin/claude.cmd".to_string()),
            schema_version: 2,
        };
        let json = serde_json::to_string(&info).expect("serialize");

        assert!(json.contains("\"dataDirectory\""), "{json}");
        assert!(json.contains("\"databasePath\""), "{json}");
        assert!(json.contains("\"databaseSizeBytes\""), "{json}");
        assert!(json.contains("\"claudeCodePath\""), "{json}");
        assert!(json.contains("\"schemaVersion\""), "{json}");
    }
}