//! AI Coding Workspace - Rust core.
//!
//! The native layer owns everything the browser-like UI must not: agent
//! adapters, per-session process environments, PTYs, process lifecycle,
//! SQLite persistence, and OS-native secret storage (spec sections 3, 4, 7).
//!
//! Module layout follows spec section 19:
//! - `agents`:      AgentAdapter trait + Claude Code implementation
//! - `providers`:   provider profile management (metadata only)
//! - `sessions`:    per-workspace session manager (one session per tab)
//! - `workspaces`:  workspace (local project) management
//! - `process`:     agent process lifecycle (spec section 15)
//! - `pty`:         pseudo-terminal handling for interactive agent CLIs
//! - `secrets`:     SecretStore trait + OS keyring implementation
//! - `persistence`: SQLite storage layer
//! - `commands`:    Tauri command handlers exposed to the React frontend
//!
//! Milestone 2 wired persistence, secrets, providers, and the provider command
//! surface. Milestone 3 added the PTY-backed session runtime: `process` (the
//! lifecycle state machine), `pty` (a PTY per session), `sessions` (the session
//! manager with per-session environment isolation), and the session commands
//! plus the `session-output:` / `session-state:` event bridge. Milestone 4 added
//! workspace management: the `WorkspaceManager` rules in `workspaces`, the
//! workspace commands (create/update/delete plus the remembered tab set), and the
//! `tauri-plugin-dialog` folder picker the "New Workspace" dialog uses - which is
//! what makes a workspace persistable, and therefore what makes `start_session`
//! reachable from the UI. See IMPLEMENTATION_PROGRESS.md.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Manager, RunEvent};

use crate::commands::agents as agent_commands;
use crate::commands::providers as provider_commands;
use crate::commands::sessions as session_commands;
use crate::commands::settings as settings_commands;
use crate::commands::workspaces as workspace_commands;
use crate::commands::AppState;
use crate::persistence::{Storage, DATABASE_FILE_NAME};
use crate::secrets::keyring_store::KeyringStore;
use crate::sessions::{SessionManager, SESSIONS_DIRECTORY_NAME};

pub mod agents;
pub mod commands;
pub mod persistence;
pub mod process;
pub mod providers;
pub mod pty;
pub mod secrets;
pub mod sessions;
pub mod workspaces;

/// How long sessions are given to stop gracefully when the application exits.
///
/// Short: the user has already decided to quit, and anything that does not stop
/// in this window is terminated forcibly right afterwards (spec section 15).
pub const SESSION_EXIT_GRACE_PERIOD: Duration = Duration::from_millis(1200);

/// Open (creating if necessary) the application database at `path`.
///
/// Split out of [`run`] so the failure the user sees when their persistence is
/// unusable - a corrupt file, an unwritable directory, a missing C++ runtime -
/// is a plain, testable function of the path and the storage error (spec
/// sections 16, 23). The message names the file, because "database error" alone
/// leaves the user with nothing to act on, and it carries no configuration:
/// secrets never reach the persistence layer at all (spec section 17).
///
/// Failing loudly is deliberate. A silent fallback to an in-memory database
/// would leave a user believing their provider profiles were saved.
pub fn open_database(path: &std::path::Path) -> Result<Storage, String> {
    Storage::open(path).map_err(|error| {
        format!(
            "could not open the application database at {}: {error}",
            path.display()
        )
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // `log` is used by the persistence layer (migrations), the session manager
    // and the PTY reader. Level defaults to `info` and can be raised with
    // RUST_LOG. Nothing logged here ever contains a credential (spec section 17).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Native folder picker for the "New Workspace" dialog (spec section 12).
        // The frontend opens it through `@tauri-apps/plugin-dialog`; the
        // `dialog:allow-open` capability in `capabilities/default.json` is what
        // permits that call. Only `open` is granted - this application never
        // shows native message/confirm dialogs.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The database lives in the platform app-data directory:
            // %APPDATA%\com.ryanheida.aiworkspace on Windows,
            // ~/Library/Application Support/... on macOS, ~/.local/share/... on
            // Linux. Tauri's resolver handles the platform differences (spec
            // section 18); `Storage::open` itself stays plain-path and testable.
            let data_directory = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_directory)?;

            let database_path = data_directory.join(DATABASE_FILE_NAME);
            let storage = open_database(&database_path).map_err(|message| {
                // Fail loudly instead of silently falling back to an in-memory
                // database: a user whose persistence is broken must not be left
                // believing their provider profiles were saved.
                std::io::Error::new(std::io::ErrorKind::Other, message)
            })?;
            log::info!(
                "application database ready (schema v{}) at {}",
                storage.schema_version().unwrap_or_default(),
                database_path.display()
            );

            // One shared secret store; providers address their keyring entry by
            // profile id. Swapping in `InMemorySecretStore` here is the only
            // change needed for environments without an OS credential store.
            app.manage(Mutex::new(AppState::new(
                storage,
                Box::new(KeyringStore::new()),
            )));

            // Paths the Settings tab reports (spec section 12). Resolved here
            // so the About / Storage section never re-derives them.
            app.manage(settings_commands::AppPaths {
                data_directory: data_directory.clone(),
                database_path: database_path.clone(),
            });

            // Session runtime (Milestone 3). Per-session configuration lives
            // below the app data directory (spec section 8); the listener
            // bridges output and state changes onto Tauri events (spec sections
            // 11, 15).
            let sessions_directory = data_directory.join(SESSIONS_DIRECTORY_NAME);
            let listener = Arc::new(session_commands::TauriSessionListener::new(app.handle().clone()));
            app.manage(SessionManager::new(sessions_directory, listener));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            agent_commands::list_agents,
            provider_commands::list_providers,
            provider_commands::create_provider,
            provider_commands::update_provider,
            provider_commands::delete_provider,
            provider_commands::set_provider_secret,
            provider_commands::provider_secret_status,
            provider_commands::test_provider,
            workspace_commands::list_workspaces,
            workspace_commands::create_workspace,
            workspace_commands::update_workspace,
            workspace_commands::delete_workspace,
            workspace_commands::load_workspace_layout,
            workspace_commands::save_workspace_layout,
            session_commands::start_session,
            session_commands::stop_session,
            session_commands::restart_session,
            session_commands::write_session,
            session_commands::resize_session,
            session_commands::list_sessions,
            settings_commands::list_ui_preferences,
            settings_commands::set_ui_preference,
            settings_commands::app_info,
            settings_commands::open_data_directory,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the tauri application");

    // Orphan prevention (spec section 15): every agent process runs inside a
    // PTY this application owns, so shutting the application down stops them all
    // - gracefully first, then forcibly. `Exit` is the last event before the
    // process ends; `ExitRequested` also fires when the last window closes, and
    // both paths are idempotent, so a session is never left behind either way.
    app.run(|app_handle, event| match event {
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            session_commands::stop_all_sessions(app_handle);
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::TempDir;

    /// A value that looks like a credential, used to prove that a persistence
    /// failure never echoes what it was reading (spec section 17).
    const SECRET_LOOKING: &str = "sk-DO-NOT-LEAK-3f19";

    #[test]
    fn a_usable_database_path_opens_and_is_migrated() {
        let directory = TempDir::new("lib-database-ok");
        let path = directory.join("nested").join(DATABASE_FILE_NAME);

        let storage = open_database(&path).expect("open a database in a fresh directory");
        assert!(path.is_file(), "the database file must have been created");
        assert!(storage.schema_version().unwrap() > 0);
    }

    #[test]
    fn a_corrupt_database_file_is_reported_with_its_path_and_nothing_else() {
        // Spec section 16: a SQLite error must reach the user as something they
        // can act on - which file, and that it is the database that is broken.
        let directory = TempDir::new("lib-database-corrupt");
        let path = directory.join(DATABASE_FILE_NAME);
        // Not a SQLite file at all: SQLite reports "file is not a database"
        // when the first page is read (i.e. during migration).
        std::fs::write(&path, format!("{SECRET_LOOKING}\nnot a database").as_bytes())
            .expect("write a corrupt database file");

        let message = open_database(&path).expect_err("a corrupt database must fail loudly");
        assert!(
            message.contains(&path.display().to_string()),
            "the message must name the file: {message}"
        );
        assert!(
            message.contains("could not open the application database"),
            "unexpected message: {message}"
        );
        assert!(message.contains("database error"), "unexpected message: {message}");
        // The failure text comes from SQLite, which describes the *schema*, not
        // the file's contents - so a credential inside a corrupt file cannot be
        // reflected back to the user.
        assert!(!message.contains(SECRET_LOOKING), "leaked: {message}");
    }

    #[test]
    fn a_database_path_that_cannot_be_created_is_reported_with_its_path() {
        // A *file* where the database's parent directory needs to be: the file
        // system refuses, and the user is told which path failed.
        let directory = TempDir::new("lib-database-blocked");
        let blocker = directory.join("blocked");
        std::fs::write(&blocker, b"not a folder").unwrap();
        let path = blocker.join(DATABASE_FILE_NAME);

        let message = open_database(&path).expect_err("an unusable path must fail");
        assert!(
            message.contains(&path.display().to_string()),
            "the message must name the file: {message}"
        );
        assert!(message.contains("database file error"), "unexpected message: {message}");
        assert!(!message.contains(SECRET_LOOKING), "leaked: {message}");
    }

    #[test]
    fn a_directory_as_the_database_path_is_reported_rather_than_panicking() {
        let directory = TempDir::new("lib-database-is-a-directory");

        let message = open_database(directory.path()).expect_err("a folder is not a database");
        assert!(message.contains("could not open the application database"));
        assert!(
            message.contains(&directory.path().display().to_string()),
            "the message must name the path: {message}"
        );
    }
}
