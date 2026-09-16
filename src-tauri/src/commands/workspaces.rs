//! Workspace management commands (spec sections 9, 10, 12, 14).
//!
//! The command surface behind the tab bar and the "New Workspace" dialog:
//!
//! | command | purpose |
//! | --- | --- |
//! | [`list_workspaces`] | every configured workspace (the tab bar's reopen list) |
//! | [`create_workspace`] | add a configured project |
//! | [`update_workspace`] | rename it, or repoint folder/agent/provider/model |
//! | [`delete_workspace`] | remove its **configuration** (never the project files) |
//! | [`load_workspace_layout`] | the tab set that was open last time |
//! | [`save_workspace_layout`] | remember the tab set (a UI preference) |
//!
//! Validation lives in [`crate::workspaces::WorkspaceManager`], so these handlers
//! only marshal arguments and convert errors to messages: the project folder must
//! exist and be a directory, the agent must be one this build implements, and the
//! provider must exist (spec section 16). Nothing here returns or logs a secret -
//! a workspace references a provider by id and never carries a credential (spec
//! section 17).
//!
//! There is deliberately no `pick_project_folder` command: the folder picker is
//! `tauri-plugin-dialog`'s `open({ directory: true })`, called from the frontend,
//! with an editable path field as the manual fallback.

use std::sync::Mutex;

use tauri::State;

use crate::commands::{error_message, lock_state, AppState};
use crate::sessions::SessionManager;
use crate::workspaces::{
    Workspace, WorkspaceError, WorkspaceInput, WorkspaceLayout, WorkspaceManager,
};
use crate::SESSION_EXIT_GRACE_PERIOD;

/// Every configured workspace, in creation order.
#[tauri::command]
pub fn list_workspaces(state: State<'_, Mutex<AppState>>) -> Result<Vec<Workspace>, String> {
    let app = lock_state(&state)?;
    list_workspaces_in(&app)
}

/// Create a workspace (spec sections 9, 12).
#[tauri::command]
pub fn create_workspace(
    state: State<'_, Mutex<AppState>>,
    input: WorkspaceInput,
) -> Result<Workspace, String> {
    let app = lock_state(&state)?;
    create_workspace_in(&app, &input)
}

/// Update a workspace's name, project folder, agent, provider or model.
#[tauri::command]
pub fn update_workspace(
    state: State<'_, Mutex<AppState>>,
    id: String,
    input: WorkspaceInput,
) -> Result<Workspace, String> {
    let app = lock_state(&state)?;
    update_workspace_in(&app, &id, &input)
}

/// Remove a workspace from the application.
///
/// `async` because it stops that workspace's live session first (graceful, then
/// forced), which can take up to the exit grace period - that must not run on the
/// UI thread. Removing a workspace never deletes the project folder, any file
/// inside it, the provider profile, or the keyring entry (spec sections 9, 14).
#[tauri::command(async)]
pub fn delete_workspace(
    state: State<'_, Mutex<AppState>>,
    sessions: State<'_, SessionManager>,
    id: String,
) -> Result<(), String> {
    // The session stop happens *before* the configuration row is removed, and
    // without the application state lock held: an agent must never keep running
    // against configuration the user just deleted, and stopping it must not
    // block every other command for the duration.
    stop_sessions_for_workspace(&sessions, &id);

    let app = lock_state(&state)?;
    delete_workspace_in(&app, &id)
}

/// The tab set that was open when the application last closed (spec section 14).
#[tauri::command]
pub fn load_workspace_layout(
    state: State<'_, Mutex<AppState>>,
) -> Result<WorkspaceLayout, String> {
    let app = lock_state(&state)?;
    load_workspace_layout_in(&app)
}

/// Remember which workspace tabs are open, so they can be restored next start.
///
/// A preference only: restoring tabs opens no process (spec section 14 - agent
/// processes are never started automatically).
#[tauri::command]
pub fn save_workspace_layout(
    state: State<'_, Mutex<AppState>>,
    layout: WorkspaceLayout,
) -> Result<WorkspaceLayout, String> {
    let app = lock_state(&state)?;
    save_workspace_layout_in(&app, &layout)
}

// --- plain functions behind the commands --------------------------------------
//
// Tauri state cannot be constructed in a unit test, so the whole body of each
// command lives in a function taking `&AppState`. The command is then a two-line
// wrapper and the behaviour is testable (spec section 23).

/// [`list_workspaces`] without the Tauri state wrapper.
pub(crate) fn list_workspaces_in(app: &AppState) -> Result<Vec<Workspace>, String> {
    WorkspaceManager::new(&app.storage)
        .list()
        .map_err(error_message)
}

/// [`create_workspace`] without the Tauri state wrapper.
pub(crate) fn create_workspace_in(
    app: &AppState,
    input: &WorkspaceInput,
) -> Result<Workspace, String> {
    WorkspaceManager::new(&app.storage)
        .create(input)
        .map_err(error_message)
}

/// [`update_workspace`] without the Tauri state wrapper.
pub(crate) fn update_workspace_in(
    app: &AppState,
    id: &str,
    input: &WorkspaceInput,
) -> Result<Workspace, String> {
    WorkspaceManager::new(&app.storage)
        .update(id, input)
        .map_err(error_message)
}

/// [`delete_workspace`] without the Tauri state wrapper.
pub(crate) fn delete_workspace_in(app: &AppState, id: &str) -> Result<(), String> {
    let manager = WorkspaceManager::new(&app.storage);
    let deleted = manager.delete(id).map_err(error_message)?;
    if !deleted {
        // Reported the way `get` does, so the UI's "workspace not found" message
        // is the same whether the row was never there or already deleted.
        return Err(error_message(WorkspaceError::NotFound(id.to_string())));
    }
    Ok(())
}

/// [`load_workspace_layout`] without the Tauri state wrapper.
pub(crate) fn load_workspace_layout_in(app: &AppState) -> Result<WorkspaceLayout, String> {
    WorkspaceManager::new(&app.storage)
        .load_layout()
        .map_err(error_message)
}

/// [`save_workspace_layout`] without the Tauri state wrapper.
pub(crate) fn save_workspace_layout_in(
    app: &AppState,
    layout: &WorkspaceLayout,
) -> Result<WorkspaceLayout, String> {
    WorkspaceManager::new(&app.storage)
        .save_layout(layout)
        .map_err(error_message)
}

/// Stop every live session of one workspace, best effort.
///
/// A failure here must not stop the workspace from being removed: the user asked
/// to remove it, and leaving configuration behind because a process was slow to
/// die would be worse. The process itself is still cleaned up by the session
/// manager's own shutdown path.
fn stop_sessions_for_workspace(sessions: &SessionManager, workspace_id: &str) {
    match sessions.stop_workspace(workspace_id, SESSION_EXIT_GRACE_PERIOD) {
        Ok(0) => {}
        Ok(stopped) => log::info!("stopped {stopped} session(s) for removed workspace {workspace_id}"),
        Err(error) => log::warn!("could not stop sessions for workspace {workspace_id}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{testing::FakeAgentAdapter, AgentAdapter};
    use crate::commands::sessions::resolve_start_request_for_tests;
    use crate::persistence::{test_support::TempDir, Storage};
    use crate::process::ProcessState;
    use crate::providers::{ProviderInput, ProviderRepository};
    use crate::secrets::memory::InMemorySecretStore;
    use crate::sessions::{SessionManager, SESSIONS_DIRECTORY_NAME};
    use std::path::PathBuf;

    struct Fixture {
        app: AppState,
        sessions: SessionManager,
        project: TempDir,
        /// Kept alive for the test's duration (the session manager's root).
        _root: TempDir,
    }

    /// An application state shaped like `run()` builds (in-memory database, no
    /// keyring) plus a real session manager, a real project folder and a real
    /// provider profile - everything a workspace needs to be startable.
    fn fixture(label: &str) -> (Fixture, String) {
        let app = AppState::new(
            Storage::open_in_memory().expect("open an in-memory database"),
            Box::new(InMemorySecretStore::new()),
        );
        let root = TempDir::new(label);
        let project = TempDir::new(&format!("{label}-project"));

        let provider = ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .expect("create a provider profile");
        app.secrets
            .set(&provider.id, "KEY_A")
            .expect("store the provider secret");

        let sessions = SessionManager::with_no_listener(root.join(SESSIONS_DIRECTORY_NAME));
        (
            Fixture {
                app,
                sessions,
                project,
                _root: root,
            },
            provider.id,
        )
    }

    /// The substitute agent the M4 tests start instead of Claude Code (spec
    /// section 23: no installation, no network).
    fn fake_agent() -> Box<dyn AgentAdapter> {
        Box::new(FakeAgentAdapter::interactive_shell())
    }

    fn input(project: &TempDir, name: &str, provider_id: &str) -> WorkspaceInput {
        WorkspaceInput {
            name: name.to_string(),
            project_path: project.path().display().to_string(),
            agent_id: "claude-code".to_string(),
            provider_id: provider_id.to_string(),
            model: None,
        }
    }

    #[test]
    fn a_workspace_created_by_the_commands_is_startable() {
        // The Milestone 4 acceptance path, end to end and without a Claude Code
        // installation: create a workspace through the command layer, resolve it
        // the way `start_session` does, and run a real PTY session from it.
        let (fixture, provider_id) = fixture("workspace-command-start");

        let created = create_workspace_in(
            &fixture.app,
            &input(&fixture.project, "Project Alpha", &provider_id),
        )
        .expect("create a workspace");

        // The list the tab bar renders contains it.
        assert_eq!(list_workspaces_in(&fixture.app).unwrap(), vec![created.clone()]);

        // Resolution: the persisted row + provider profile + keyring secret,
        // with the agent substituted for a harmless real program.
        let request = resolve_start_request_for_tests(&fixture.app, &created.id, fake_agent)
            .expect("resolve the workspace into a start request");
        assert_eq!(request.workspace_id, created.id);
        assert_eq!(request.project_path, PathBuf::from(&created.project_path));
        assert_eq!(request.provider.base_url, "https://provider-a.example.com");
        assert_eq!(request.provider.api_key(), Some("KEY_A"));

        // A real PTY session, in the workspace's project folder.
        let info = fixture
            .sessions
            .start(request)
            .expect("start the session for the persisted workspace");
        assert_eq!(info.workspace_id, created.id);
        assert_eq!(info.status, ProcessState::Running);
        assert_eq!(
            fixture
                .sessions
                .working_directory_for_testing(&info.id)
                .unwrap(),
            fixture.project.path()
        );

        // It accepts input and stops cleanly.
        fixture.sessions.write(&info.id, "exit\r\n").ok();
        fixture
            .sessions
            .stop_with_grace(&info.id, std::time::Duration::from_millis(200))
            .expect("stop the session");
        assert_eq!(
            fixture.sessions.get(&info.id).unwrap().status,
            ProcessState::Stopped
        );
    }

    #[test]
    fn validation_errors_reach_the_caller_as_plain_messages() {
        let (fixture, provider_id) = fixture("workspace-command-validation");

        // Empty name.
        let empty = create_workspace_in(
            &fixture.app,
            &WorkspaceInput {
                name: "   ".to_string(),
                ..input(&fixture.project, "ignored", &provider_id)
            },
        )
        .expect_err("an empty name must be refused");
        assert_eq!(empty, "workspace name must not be empty");

        // Project folder that is not a directory.
        let missing = create_workspace_in(
            &fixture.app,
            &WorkspaceInput {
                project_path: fixture.project.join("nope").display().to_string(),
                ..input(&fixture.project, "Project Alpha", &provider_id)
            },
        )
        .expect_err("a missing folder must be refused");
        assert!(
            missing.starts_with("the project folder does not exist: "),
            "unexpected message: {missing}"
        );

        // Unknown provider.
        let unknown = create_workspace_in(
            &fixture.app,
            &input(&fixture.project, "Project Alpha", "no-such-provider"),
        )
        .expect_err("an unknown provider must be refused");
        assert_eq!(unknown, "provider not found: no-such-provider");

        // Nothing was persisted, and each message is secret-free (spec 16/17).
        assert!(list_workspaces_in(&fixture.app).unwrap().is_empty());
        assert!(!missing.contains("KEY_A"));
    }

    #[test]
    fn renaming_a_workspace_keeps_its_id_and_its_project_folder() {
        let (fixture, provider_id) = fixture("workspace-command-rename");
        let created = create_workspace_in(
            &fixture.app,
            &input(&fixture.project, "Project Alpha", &provider_id),
        )
        .unwrap();

        let renamed = update_workspace_in(
            &fixture.app,
            &created.id,
            &WorkspaceInput {
                name: "Renamed Alpha".to_string(),
                model: Some("model-b".to_string()),
                ..input(&fixture.project, "Project Alpha", &provider_id)
            },
        )
        .expect("rename the workspace");

        assert_eq!(renamed.id, created.id, "a rename must not change the id");
        assert_eq!(renamed.name, "Renamed Alpha");
        assert_eq!(renamed.project_path, created.project_path);
        assert_eq!(renamed.model, Some("model-b".to_string()));
        assert_eq!(list_workspaces_in(&fixture.app).unwrap(), vec![renamed]);

        // An update of a workspace that does not exist is a clear error.
        let missing = update_workspace_in(
            &fixture.app,
            "no-such-workspace",
            &input(&fixture.project, "Project Alpha", &provider_id),
        )
        .expect_err("updating a missing workspace must fail");
        assert_eq!(missing, "workspace not found: no-such-workspace");
    }

    #[test]
    fn deleting_a_workspace_stops_its_session_and_deletes_nothing_else() {
        let (fixture, provider_id) = fixture("workspace-command-delete");
        let created = create_workspace_in(
            &fixture.app,
            &input(&fixture.project, "Project Alpha", &provider_id),
        )
        .unwrap();

        // A file in the project folder, and a live session for the workspace.
        let marker = fixture.project.join("marker.txt");
        std::fs::write(&marker, b"user data").unwrap();

        let request = resolve_start_request_for_tests(&fixture.app, &created.id, fake_agent)
            .unwrap();
        let info = fixture.sessions.start(request).unwrap();
        assert_eq!(info.status, ProcessState::Running);

        // Delete through the wiring `delete_workspace` uses.
        stop_sessions_for_workspace(&fixture.sessions, &created.id);
        delete_workspace_in(&fixture.app, &created.id).expect("delete the workspace");

        // The agent process is gone with it (spec section 15).
        assert_eq!(
            fixture.sessions.get(&info.id).unwrap().status,
            ProcessState::Stopped
        );

        // Configuration removed...
        assert!(list_workspaces_in(&fixture.app).unwrap().is_empty());
        assert_eq!(
            delete_workspace_in(&fixture.app, &created.id).unwrap_err(),
            format!("workspace not found: {}", created.id)
        );

        // ...and nothing else: project files, provider profile and credential.
        assert!(marker.is_file());
        assert_eq!(std::fs::read(&marker).unwrap(), b"user data");
        assert!(ProviderRepository::new(&fixture.app.storage)
            .find(&provider_id)
            .unwrap()
            .is_some());
        assert_eq!(
            fixture.app.secrets.get(&provider_id).unwrap().as_deref(),
            Some("KEY_A"),
            "deleting a workspace must not remove a provider credential"
        );
    }

    #[test]
    fn the_tab_layout_is_stored_through_the_command_layer() {
        let (fixture, _provider_id) = fixture("workspace-command-layout");

        // Fresh database: nothing remembered, and the payload is camelCase.
        assert_eq!(
            load_workspace_layout_in(&fixture.app).unwrap(),
            WorkspaceLayout::default()
        );

        let saved = save_workspace_layout_in(
            &fixture.app,
            &WorkspaceLayout {
                open_workspace_ids: vec!["workspace-a".to_string(), "workspace-b".to_string()],
                active_workspace_id: Some("workspace-b".to_string()),
            },
        )
        .unwrap();
        assert_eq!(load_workspace_layout_in(&fixture.app).unwrap(), saved);

        // The restored tab set is configuration-only: no session was started.
        assert!(fixture.sessions.list().unwrap().is_empty());
    }
}
