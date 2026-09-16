//! Session commands (spec sections 8, 10, 11, 15).
//!
//! The command surface the React frontend already calls through
//! `src/services/sessions.ts`:
//!
//! | command | purpose |
//! | --- | --- |
//! | [`start_session`] | spawn the agent for a workspace |
//! | [`stop_session`] | graceful stop, then forced |
//! | [`restart_session`] | stop + spawn again under the same session id |
//! | [`write_session`] | forward keystrokes to the PTY |
//! | [`resize_session`] | forward the terminal's geometry |
//! | [`list_sessions`] | every known session (re-attach after a UI reload) |
//!
//! Live output and lifecycle transitions are pushed, not polled:
//! [`TauriSessionListener`] bridges [`SessionListener`] onto the Tauri event
//! channels `session-output:<sessionId>` (a bare string of output) and
//! `session-state:<sessionId>` (`{ sessionId, status, exitCode }`) - exactly the
//! shape `src/services/sessions.ts` subscribes to.
//!
//! Nothing here returns secret material. The credential is read from the OS
//! keyring, moved into the session's environment, and never crosses the Tauri
//! boundary (spec sections 7, 17).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::agents::claude_code::{ClaudeCodeAdapter, AGENT_ID};
use crate::agents::AgentAdapter;
use crate::commands::{error_message, lock_state, AppState};
use crate::providers::{ProviderRepository, ResolvedProvider};
use crate::sessions::{
    decode_output_chunk, SessionId, SessionInfo, SessionListener, SessionManager,
    SessionStateEvent, StartSessionRequest,
};
use crate::workspaces::WorkspaceRepository;

/// Event channel carrying raw PTY output for one session.
pub fn session_output_channel(session_id: &str) -> String {
    format!("session-output:{session_id}")
}

/// Event channel carrying lifecycle transitions for one session.
pub fn session_state_channel(session_id: &str) -> String {
    format!("session-state:{session_id}")
}

/// Bridges the Tauri event API onto the session manager's listener.
///
/// PTY bytes are decoded to text here (with a carry buffer per session, so a
/// UTF-8 character split across two reads survives) because the frontend
/// terminal consumes strings. `emit` failures are logged at debug level only: a
/// window that closed mid-session must not make the reader thread noisy.
pub struct TauriSessionListener {
    app: AppHandle,
    /// Per-session trailing bytes of an incomplete UTF-8 sequence.
    pending: Mutex<HashMap<String, Vec<u8>>>,
}

impl TauriSessionListener {
    /// Wrap the application handle whose webviews receive the events.
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            pending: Mutex::new(HashMap::new()),
        }
    }
}

impl SessionListener for TauriSessionListener {
    fn on_output(&self, session_id: &SessionId, chunk: &[u8]) {
        let text = match self.pending.lock() {
            Ok(mut pending) => {
                let carry = pending.entry(session_id.as_str().to_string()).or_default();
                decode_output_chunk(carry, chunk)
            }
            // A poisoned carry buffer must not swallow output.
            Err(_) => String::from_utf8_lossy(chunk).into_owned(),
        };
        if text.is_empty() {
            return;
        }
        if let Err(error) = self
            .app
            .emit(&session_output_channel(session_id.as_str()), text)
        {
            log::debug!("could not deliver session output: {error}");
        }
    }

    fn on_state(&self, event: &SessionStateEvent) {
        if let Err(error) = self
            .app
            .emit(&session_state_channel(&event.session_id), event.clone())
        {
            log::debug!("could not deliver a session state change: {error}");
        }
    }
}

/// Spawn the configured agent for a workspace (spec sections 6-8).
///
/// The workspace row supplies the project folder, the agent and the provider;
/// the provider's credential comes from the OS keyring and is applied to this
/// session's child process only - the application's own environment is never
/// modified (spec section 7).
#[tauri::command(async)]
pub fn start_session(
    state: State<'_, Mutex<AppState>>,
    sessions: State<'_, SessionManager>,
    workspace_id: String,
) -> Result<SessionInfo, String> {
    // Resolve everything database- and secret-related while the state lock is
    // held, then release it before the (slower) PTY spawn. `async` on purpose:
    // spawning and, above all, stopping must not run on the UI thread.
    let request = {
        let app = lock_state(&state)?;
        start_request(&app, &workspace_id)?
    };
    sessions.start(request).map_err(error_message)
}

/// Stop a session: graceful first (interrupt + EOF), then forced (spec section 15).
#[tauri::command(async)]
pub fn stop_session(
    sessions: State<'_, SessionManager>,
    session_id: String,
) -> Result<(), String> {
    sessions.stop(&session_id).map_err(error_message)
}

/// Stop and start again, keeping the session id the frontend is holding.
#[tauri::command(async)]
pub fn restart_session(
    sessions: State<'_, SessionManager>,
    session_id: String,
) -> Result<SessionInfo, String> {
    sessions.restart(&session_id).map_err(error_message)
}

/// Forward terminal input (keystrokes, pasted text, escape sequences) to the PTY.
#[tauri::command]
pub fn write_session(
    sessions: State<'_, SessionManager>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    sessions.write(&session_id, &data).map_err(error_message)
}

/// Tell the session's PTY its new geometry so full-screen TUIs reflow.
#[tauri::command]
pub fn resize_session(
    sessions: State<'_, SessionManager>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    sessions
        .resize(&session_id, cols, rows)
        .map_err(error_message)
}

/// Every known session, so a reloaded UI can re-attach (spec section 14).
#[tauri::command]
pub fn list_sessions(sessions: State<'_, SessionManager>) -> Result<Vec<SessionInfo>, String> {
    sessions.list().map_err(error_message)
}

/// Resolve the workspace, provider and credential for a start request.
///
/// Model precedence: the workspace's optional model override wins over the
/// provider's default model (spec section 9: a workspace carries its own
/// session configuration).
fn start_request(app: &AppState, workspace_id: &str) -> Result<StartSessionRequest, String> {
    resolve_start_request(app, workspace_id, &claude_code_adapter)
}

/// The agent adapter for a persisted `agent_id` (spec sections 6, 24).
///
/// The only adapter this build implements. Kept as a function rather than an
/// inline `match` so [`resolve_start_request`] can be handed a different factory
/// by tests, which is how a persisted workspace is proven startable end to end
/// without a Claude Code installation (`FakeAgentAdapter`).
fn claude_code_adapter(agent_id: &str) -> Result<Box<dyn AgentAdapter>, String> {
    match agent_id {
        AGENT_ID => Ok(Box::new(ClaudeCodeAdapter::new())),
        // Only Claude Code is implemented for the MVP (spec sections 6, 24);
        // the message names the id so a future adapter's absence is obvious.
        other => Err(format!("unsupported agent: {other}")),
    }
}

/// Resolve the workspace, provider and credential for a start request, with a
/// substitute agent.
///
/// Test-only: the M4 command tests use it to prove that a workspace created
/// through the workspace commands resolves to a startable session without a
/// Claude Code installation (spec section 23). The agent id is ignored on
/// purpose - the production `match` above is exercised by the tests in this
/// module.
#[cfg(test)]
pub(crate) fn resolve_start_request_for_tests(
    app: &AppState,
    workspace_id: &str,
    make_adapter: impl Fn() -> Box<dyn AgentAdapter>,
) -> Result<StartSessionRequest, String> {
    resolve_start_request(app, workspace_id, &|_agent_id| Ok(make_adapter()))
}

/// Resolve a workspace row into everything [`SessionManager::start`] needs.
///
/// `make_adapter` chooses the agent to run; everything else - the workspace, its
/// provider profile, and the credential from the secret store - comes from the
/// application state. This is the seam between "what is persisted" (M4) and "what
/// runs" (M3), and it is why a workspace created through the M4 commands is
/// startable.
fn resolve_start_request(
    app: &AppState,
    workspace_id: &str,
    make_adapter: &dyn Fn(&str) -> Result<Box<dyn AgentAdapter>, String>,
) -> Result<StartSessionRequest, String> {
    let workspace = WorkspaceRepository::new(&app.storage)
        .find(workspace_id)
        .map_err(error_message)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;

    let profile = ProviderRepository::new(&app.storage)
        .find(&workspace.provider_id)
        .map_err(error_message)?
        .ok_or_else(|| {
            format!(
                "provider not found for workspace {}: {}",
                workspace.name, workspace.provider_id
            )
        })?;

    // Read the credential last, immediately before it is handed to the session,
    // and never log or return it (spec sections 5, 17).
    let api_key = app.secrets.get(&profile.id).map_err(error_message)?;
    let mut provider = ResolvedProvider::new(profile, api_key);
    if let Some(model) = workspace.model.as_ref().map(|model| model.trim()) {
        if !model.is_empty() {
            provider.model = model.to_string();
        }
    }

    let adapter = make_adapter(workspace.agent_id.trim())?;

    Ok(StartSessionRequest {
        workspace_id: workspace.id,
        project_path: PathBuf::from(workspace.project_path),
        provider,
        adapter,
    })
}

/// Stop every session when the application exits (spec section 15: no orphaned
/// agent processes).
///
/// Wired into the Tauri run loop from [`crate::run`]. Safe to call more than
/// once: a finished session is left alone.
pub fn stop_all_sessions(app: &AppHandle) {
    let Some(sessions) = app.try_state::<SessionManager>() else {
        return;
    };
    log::info!("stopping {} session(s) before exit", sessions.len());
    sessions.stop_all(crate::SESSION_EXIT_GRACE_PERIOD);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Storage;
    use crate::providers::{ProviderInput, ProviderRepository};
    use crate::secrets::memory::InMemorySecretStore;
    use crate::workspaces::{WorkspaceInput, WorkspaceRepository};

    /// The app state assembled here is the same shape `run()` builds, minus
    /// Tauri: a real (in-memory) database and an in-memory secret store, so no
    /// test touches a developer's keyring.
    fn app_state() -> AppState {
        AppState::new(
            Storage::open_in_memory().expect("open an in-memory database"),
            Box::new(InMemorySecretStore::new()),
        )
    }

    /// A workspace bound to a provider, with the provider's key stored.
    fn workspace_with_provider(
        app: &AppState,
        agent_id: &str,
        model: Option<&str>,
    ) -> (String, String) {
        let profile = ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .expect("create a provider");
        app.secrets
            .set(&profile.id, "KEY_A")
            .expect("store the provider secret");

        let workspace = WorkspaceRepository::new(&app.storage)
            .create(&WorkspaceInput {
                name: "Project Alpha".to_string(),
                project_path: "D:\\Work\\project-alpha".to_string(),
                agent_id: agent_id.to_string(),
                provider_id: profile.id.clone(),
                model: model.map(str::to_string),
            })
            .expect("create a workspace");

        (workspace.id, profile.id)
    }

    #[test]
    fn event_channels_match_the_frontend_subscriptions() {
        // `services/sessions.ts` subscribes to exactly these names.
        assert_eq!(session_output_channel("abc"), "session-output:abc");
        assert_eq!(session_state_channel("abc"), "session-state:abc");
    }

    #[test]
    fn a_start_request_resolves_the_workspace_provider_and_keyring_secret() {
        let app = app_state();
        let (workspace_id, provider_id) = workspace_with_provider(&app, AGENT_ID, None);

        let request = start_request(&app, &workspace_id).expect("resolve the start request");

        assert_eq!(request.workspace_id, workspace_id);
        assert_eq!(request.project_path, PathBuf::from("D:\\Work\\project-alpha"));
        // The provider profile and the credential from the secret store are
        // joined only here, in memory (spec sections 5, 7).
        assert_eq!(request.provider.id, provider_id);
        assert_eq!(request.provider.base_url, "https://provider-a.example.com");
        assert_eq!(request.provider.model, "model-a");
        assert_eq!(request.provider.api_key(), Some("KEY_A"));
        assert_eq!(request.adapter.id(), AGENT_ID);
        // The secret must not be printable (spec section 17).
        assert!(!format!("{request:?}").contains("KEY_A"));
    }

    #[test]
    fn a_workspace_model_override_wins_over_the_provider_default() {
        let app = app_state();
        let (workspace_id, _provider_id) = workspace_with_provider(&app, AGENT_ID, Some("model-a-override"));

        let request = start_request(&app, &workspace_id).unwrap();
        assert_eq!(request.provider.model, "model-a-override");

        // A blank override means "use the provider default".
        let (blank_id, _provider_id) = workspace_with_provider(&app, AGENT_ID, Some("   "));
        let request = start_request(&app, &blank_id).unwrap();
        assert_eq!(request.provider.model, "model-a");
    }

    #[test]
    fn a_start_request_without_a_stored_key_is_still_valid() {
        // Some gateways need no credential; the adapter simply leaves
        // ANTHROPIC_AUTH_TOKEN unset (spec section 5).
        let app = app_state();
        let profile = ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .unwrap();
        let workspace = WorkspaceRepository::new(&app.storage)
            .create(&WorkspaceInput {
                name: "Project Alpha".to_string(),
                project_path: "D:\\Work\\project-alpha".to_string(),
                agent_id: AGENT_ID.to_string(),
                provider_id: profile.id,
                model: None,
            })
            .unwrap();

        let request = start_request(&app, &workspace.id).unwrap();
        assert_eq!(request.provider.api_key(), None);
    }

    #[test]
    fn start_requests_report_missing_or_unsupported_configuration() {
        let app = app_state();

        // Unknown workspace (the M1 seed tabs land here until M4 persists real
        // workspaces - the message must say so rather than failing obscurely).
        let error = start_request(&app, "project-alpha").expect_err("unknown workspace");
        assert_eq!(error, "workspace not found: project-alpha");

        // Workspace pointing at a provider that no longer exists.
        let profile = ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .unwrap();
        let orphan = WorkspaceRepository::new(&app.storage)
            .create(&WorkspaceInput {
                name: "Project Alpha".to_string(),
                project_path: "D:\\Work\\project-alpha".to_string(),
                agent_id: AGENT_ID.to_string(),
                provider_id: profile.id.clone(),
                model: None,
            })
            .unwrap();
        ProviderRepository::new(&app.storage)
            .delete(&profile.id)
            .unwrap();
        let error = start_request(&app, &orphan.id).expect_err("missing provider");
        assert!(error.starts_with("provider not found for workspace Project Alpha"), "{error}");

        // An agent this build does not implement (spec section 24: Claude Code only).
        let (workspace_id, _provider_id) = workspace_with_provider(&app, "codex", None);
        let error = start_request(&app, &workspace_id).expect_err("unsupported agent");
        assert_eq!(error, "unsupported agent: codex");
    }

    /// The command boundary's half of spec sections 16 and 17: every way a
    /// start can fail must produce a plain, understandable message, and no
    /// failure may carry the credential the session was about to use.
    ///
    /// The credential is a real stored secret (not a literal in the workspace
    /// row), so this covers the whole path a user's key actually travels:
    /// keyring, `ResolvedProvider`, session environment - and back out through
    /// an error.
    #[test]
    fn no_session_start_failure_carries_the_provider_credential() {
        use crate::agents::testing::FakeAgentAdapter;
        use crate::persistence::test_support::TempDir;
        use crate::sessions::{SessionManager, SESSIONS_DIRECTORY_NAME};

        const SECRET: &str = "sk-DO-NOT-LEAK-9c41fb7e";

        /// Assert a failure message - and the error behind it - never contains
        /// the credential (spec section 17).
        fn assert_secret_free(label: &str, message: &str) {
            assert!(!message.is_empty(), "{label}: a failure must explain itself");
            assert!(
                !message.contains(SECRET),
                "{label}: the message leaked the credential: {message}"
            );
        }

        let app = app_state();
        let root = TempDir::new("session-command-secret");
        let project = TempDir::new("session-command-secret-project");
        let sessions = SessionManager::with_no_listener(root.join(SESSIONS_DIRECTORY_NAME));
        // A stored secret with a distinctive value, so any leak is visible.
        let profile = ProviderRepository::new(&app.storage)
            .create(&ProviderInput {
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .unwrap();
        app.secrets.set(&profile.id, SECRET).unwrap();
        let workspace = WorkspaceRepository::new(&app.storage)
            .create(&WorkspaceInput {
                name: "Project Alpha".to_string(),
                project_path: project.path().display().to_string(),
                agent_id: AGENT_ID.to_string(),
                provider_id: profile.id.clone(),
                model: None,
            })
            .unwrap();

        // 1. Unknown workspace.
        assert_secret_free(
            "unknown workspace",
            &start_request(&app, "no-such-workspace").expect_err("unknown workspace"),
        );

        // 2. Missing executable (spec section 16: "Invalid executable") - the
        //    request resolves fine, and the PTY spawn is what fails.
        let request = resolve_start_request_for_tests(&app, &workspace.id, || {
            Box::new(FakeAgentAdapter::missing_program())
        })
        .expect("resolve the start request");
        let error = sessions
            .start(request)
            .expect_err("a missing executable must not start a session");
        assert_secret_free("missing executable", &error.to_string());
        assert!(
            !format!("{error:?}").contains(SECRET),
            "the error's Debug output leaked the credential: {error:?}"
        );
        assert!(sessions.list().unwrap().is_empty(), "a failed start registers nothing");

        // 3. Project folder deleted (spec section 16: "Project directory
        //    deleted") - the folder is valid at save time, so only a start can
        //    catch this.
        std::fs::remove_dir_all(project.path()).expect("delete the project folder");
        let request = resolve_start_request_for_tests(&app, &workspace.id, || {
            Box::new(FakeAgentAdapter::interactive_shell())
        })
        .expect("resolve the start request");
        let error = sessions
            .start(request)
            .expect_err("a deleted project folder must not start a session");
        let message = error.to_string();
        assert!(
            message.contains("the project folder does not exist"),
            "the failure must name the folder rather than the process: {message}"
        );
        assert!(
            message.contains(&project.path().display().to_string()),
            "the failure must name the folder that is gone: {message}"
        );
        assert_secret_free("deleted project folder", &message);
        assert!(!format!("{error:?}").contains(SECRET));
        assert!(sessions.list().unwrap().is_empty());
    }
}
