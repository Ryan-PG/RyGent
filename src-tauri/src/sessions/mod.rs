//! Session management (spec sections 8, 10, 15, 19).
//!
//! A [`SessionManager`] owns one [`SessionRuntime`] per started session. Each
//! session has its own id, workspace, agent adapter, PTY, child process,
//! environment, working directory, lifecycle state and per-session
//! configuration directory - so `Session A != Session B != Session C` even when
//! all three run the same agent (spec section 8).
//!
//! What this module guarantees:
//!
//! - **Isolation.** The environment is built per session from that workspace's
//!   provider profile plus the credential read from the secret store, and is
//!   handed to that session's child process only. The application's own
//!   environment is never written to (spec sections 7, 8), and two sessions
//!   never share a configuration directory.
//! - **Bounded control.** Start, stop (graceful first, then forced), restart,
//!   write, resize and list all return; every wait is bounded by a timeout, so
//!   no session can hang the app (spec section 15).
//! - **Honest state.** [`crate::process::ProcessState`] is the single lifecycle
//!   vocabulary, and every transition is broadcast to a [`SessionListener`] the
//!   Tauri layer bridges to frontend events.
//!
//! The module is Tauri-free on purpose: the listener is a trait, so the whole
//! manager is unit-testable without a webview (spec section 23).

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use uuid::Uuid;

use crate::agents::{AgentAdapter, AgentError, SpawnRequest};
use crate::process::{ExitInfo, ProcessSpawn, ProcessState, TransitionError};
use crate::providers::ResolvedProvider;
use crate::pty::{PtyError, PtyProcess, TerminalSize};

/// Directory (inside the app data directory) holding per-session state.
pub const SESSIONS_DIRECTORY_NAME: &str = "sessions";

/// How long a session is given to stop on its own before it is forced.
///
/// Short on purpose: this is the delay between "Stop" and the UI reporting a
/// stopped session, and a CLI that ignores EOF and Ctrl-C is not going to
/// change its mind later (spec section 15).
pub const DEFAULT_GRACE_PERIOD: Duration = Duration::from_millis(1500);

/// How long a forced termination is given to be reaped.
const FORCED_TERMINATION_TIMEOUT: Duration = Duration::from_millis(1000);

/// How often the per-session supervisor polls its child process.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(40);

/// Identifier of one session. Wraps a string so a session id cannot be
/// confused with a workspace id (both cross the Tauri boundary as strings).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    /// Generate a fresh session id.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The id as a plain string (the wire representation).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Notifications from the session manager (spec sections 11, 15).
///
/// The Tauri layer implements this to emit `session-output:<id>` and
/// `session-state:<id>` events; tests implement it to record what happened.
/// Implementations must not block: `on_output` is called from the PTY reader
/// thread, and a slow implementation throttles the agent.
pub trait SessionListener: Send + Sync {
    /// A chunk of raw PTY output for one session.
    fn on_output(&self, session_id: &SessionId, chunk: &[u8]);

    /// A lifecycle transition (or an exit code becoming known).
    fn on_state(&self, event: &SessionStateEvent);
}

/// A [`SessionListener`] that discards everything.
pub struct NoopSessionListener;

impl SessionListener for NoopSessionListener {
    fn on_output(&self, _session_id: &SessionId, _chunk: &[u8]) {}

    fn on_state(&self, _event: &SessionStateEvent) {}
}

/// Decode a PTY output chunk into text for the terminal view.
///
/// `pending` carries a trailing incomplete UTF-8 sequence across calls, so a
/// multi-byte character split across two reads is delivered whole instead of as
/// a replacement character. Truly invalid bytes become `U+FFFD`.
///
/// This is the only "terminal" interpretation this application does: the byte
/// stream is otherwise passed through untouched for `xterm.js` to render
/// (spec section 11 - never implement terminal emulation yourself).
pub(crate) fn decode_output_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    let mut buffer = std::mem::take(pending);
    buffer.extend_from_slice(chunk);

    let mut output = String::new();
    let mut rest: &[u8] = &buffer;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                output.push_str(std::str::from_utf8(&rest[..valid_up_to]).unwrap_or_default());
                match error.error_len() {
                    Some(invalid_length) => {
                        output.push('\u{FFFD}');
                        rest = &rest[valid_up_to + invalid_length..];
                    }
                    None => {
                        // Truncated tail: keep it for the next chunk.
                        *pending = rest[valid_up_to..].to_vec();
                        break;
                    }
                }
            }
        }
    }
    output
}

/// A session as reported to the frontend (spec sections 8, 15).
///
/// Serialized `camelCase` to match `SessionInfo` in `src/types/index.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Workspace this session belongs to (one live session per workspace).
    pub workspace_id: String,
    /// Current lifecycle state.
    pub status: ProcessState,
    /// Columns the PTY currently uses.
    pub cols: u16,
    /// Rows the PTY currently uses.
    pub rows: u16,
    /// Exit code once the process has exited; `null` while it lives.
    pub exit_code: Option<i32>,
}

/// Payload of the `session-state:<id>` event (spec section 15: the UI must
/// accurately reflect the process state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateEvent {
    /// Session this transition belongs to.
    pub session_id: String,
    /// New lifecycle state.
    pub status: ProcessState,
    /// Exit code, if the process has exited.
    pub exit_code: Option<i32>,
}

/// Everything needed to start one session.
///
/// Assembled by the command layer (which resolves the workspace row, the
/// provider profile and the keyring credential) so this module never has to
/// touch the database or a secret store. `Debug` is derived and stays safe
/// because [`ResolvedProvider`] redacts its key.
#[derive(Debug)]
pub struct StartSessionRequest {
    /// Workspace the session belongs to.
    pub workspace_id: String,
    /// The workspace's project folder: the agent's working directory.
    pub project_path: PathBuf,
    /// Provider profile plus the credential read from the OS keyring. Kept in
    /// memory for the lifetime of the session (a restart must not require the
    /// key again) and never persisted, logged or serialized.
    pub provider: ResolvedProvider,
    /// The agent to run.
    pub adapter: Box<dyn AgentAdapter>,
}

/// Errors produced by the session manager (spec section 16).
///
/// Messages are user-facing and secret-free: an environment value can never
/// reach them (spec section 17).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// No session has this id (or it was already cleaned up).
    #[error("session not found: {0}")]
    NotFound(String),

    /// The session exists but is not in a state that accepts the operation.
    #[error("session {id} is {state}; this operation needs a live session")]
    NotLive {
        /// Session id.
        id: String,
        /// State it was found in.
        state: ProcessState,
    },

    /// The agent could not describe a process to start.
    #[error(transparent)]
    Agent(#[from] AgentError),

    /// The PTY layer failed.
    #[error(transparent)]
    Pty(#[from] PtyError),

    /// A lifecycle transition was refused by the state machine.
    #[error(transparent)]
    Transition(#[from] TransitionError),

    /// The per-session configuration directory could not be prepared.
    #[error("could not create the session directory {path}: {message}")]
    SessionDirectory {
        /// Directory that failed.
        path: String,
        /// Platform error text.
        message: String,
    },

    /// The supervisor thread for a session could not be started.
    #[error("could not start the session supervisor: {0}")]
    Supervisor(String),

    /// Another thread panicked while holding a session lock.
    #[error("a session lock was poisoned by an earlier panic")]
    LockPoisoned,
}

/// One session's mutable runtime state.
///
/// Shared as `Arc<Mutex<..>>` so a session's supervisor thread and a command
/// thread can coordinate without ever holding the manager's registry lock
/// (which is what keeps stopping session A free of session B - spec section 10).
struct SessionRuntime {
    id: SessionId,
    /// Creation order; `list_sessions` reports sessions in a stable order.
    sequence: u64,
    workspace_id: String,
    agent_id: &'static str,
    agent_name: &'static str,
    project_path: PathBuf,
    config_directory: PathBuf,
    provider: ResolvedProvider,
    adapter: Box<dyn AgentAdapter>,
    /// The exact process description used for the current run (program, args,
    /// working directory, environment). `None` before the first successful build.
    spawn: Option<ProcessSpawn>,
    state: ProcessState,
    exit: Option<ExitInfo>,
    size: TerminalSize,
    pty: Option<PtyProcess>,
}

impl SessionRuntime {
    /// Wire representation of this session.
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.as_str().to_string(),
            workspace_id: self.workspace_id.clone(),
            status: self.state,
            cols: self.size.cols(),
            rows: self.size.rows(),
            exit_code: self.exit.as_ref().and_then(|exit| exit.code),
        }
    }

    /// Value used by [`SessionListener::on_state`].
    fn state_event(&self) -> SessionStateEvent {
        SessionStateEvent {
            session_id: self.id.as_str().to_string(),
            status: self.state,
            exit_code: self.exit.as_ref().and_then(|exit| exit.code),
        }
    }
}

impl fmt::Debug for SessionRuntime {
    /// Hand-written: the runtime holds a credential-bearing environment, and
    /// this must never render a value (spec section 17).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRuntime")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("project_path", &self.project_path)
            .field("config_directory", &self.config_directory)
            .field("state", &self.state)
            .field("exit", &self.exit)
            .field("size", &self.size)
            .field("input_environment", &self.spawn.as_ref().map(|_| "<session environment>"))
            .finish_non_exhaustive()
    }
}

/// Owns every session (spec sections 8, 10, 15).
///
/// Cheap to share: all mutable state is behind locks, the registry lock is only
/// held for lookups, and each session is locked independently. Managed as Tauri
/// state by the command layer.
pub struct SessionManager {
    /// Root directory for per-session configuration (`<app data>/sessions`).
    sessions_directory: PathBuf,
    /// Where lifecycle and output notifications go.
    listener: Arc<dyn SessionListener>,
    /// Registry. Locked only to look a session up, never while waiting on a
    /// process.
    sessions: Mutex<HashMap<SessionId, Arc<Mutex<SessionRuntime>>>>,
    /// Creation order for `list_sessions`.
    sequence: AtomicU64,
}

impl SessionManager {
    /// Create a manager writing per-session state below `sessions_directory`.
    pub fn new(
        sessions_directory: impl Into<PathBuf>,
        listener: Arc<dyn SessionListener>,
    ) -> Self {
        Self {
            sessions_directory: sessions_directory.into(),
            listener,
            sessions: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
        }
    }

    /// Create a manager that reports nothing (tests, headless use).
    pub fn with_no_listener(sessions_directory: impl Into<PathBuf>) -> Self {
        Self::new(sessions_directory, Arc::new(NoopSessionListener))
    }

    /// Start the agent for a workspace (spec sections 7, 8, 15).
    ///
    /// A workspace has at most one live session: if one is already running this
    /// returns it unchanged, which is what makes `start_session` safe to repeat
    /// after a UI reload. Finished sessions for the same workspace are removed
    /// first, releasing their PTYs.
    pub fn start(&self, request: StartSessionRequest) -> Result<SessionInfo, SessionError> {
        if let Some(existing) = self.live_session_for(&request.workspace_id)? {
            log::info!(
                "workspace {} already has a live session ({}); reusing it",
                request.workspace_id,
                existing.id
            );
            return Ok(existing);
        }
        self.remove_finished_sessions(&request.workspace_id)?;

        let session_id = SessionId::new();
        let config_directory = self.sessions_directory.join(session_id.as_str());

        let runtime = SessionRuntime {
            id: session_id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            workspace_id: request.workspace_id,
            agent_id: request.adapter.id(),
            agent_name: request.adapter.name(),
            project_path: request.project_path,
            config_directory,
            provider: request.provider,
            adapter: request.adapter,
            spawn: None,
            state: ProcessState::Created,
            exit: None,
            size: TerminalSize::default(),
            pty: None,
        };

        let shared = Arc::new(Mutex::new(runtime));
        {
            // Scope the session lock: the registry lock below is only acquired
            // after this one is released, so lock order stays registry ->
            // session everywhere.
            let mut locked = lock_runtime(&shared)?;
            self.spawn_runtime(&mut locked, &shared)?;
        }
        self.lock_registry()?.insert(session_id.clone(), Arc::clone(&shared));

        self.get(session_id.as_str())
    }

    /// Stop a session, giving it [`DEFAULT_GRACE_PERIOD`] to exit on its own.
    ///
    /// Stopping an already-finished session is a no-op, so a UI that is out of
    /// step does not get an error.
    pub fn stop(&self, session_id: &str) -> Result<(), SessionError> {
        self.stop_with_grace(session_id, DEFAULT_GRACE_PERIOD)
    }

    /// [`SessionManager::stop`] with an explicit grace period (tests use a
    /// short one so the suite stays fast).
    pub fn stop_with_grace(&self, session_id: &str, grace: Duration) -> Result<(), SessionError> {
        let shared = self.session(session_id)?;
        let mut runtime = lock_runtime(&shared)?;
        self.stop_runtime(&mut runtime, grace);
        Ok(())
    }

    /// Stop the session (if needed) and start the agent again under the same
    /// session id, so the frontend keeps addressing the same tab.
    pub fn restart(&self, session_id: &str) -> Result<SessionInfo, SessionError> {
        let shared = self.session(session_id)?;
        let mut runtime = lock_runtime(&shared)?;
        self.stop_runtime(&mut runtime, DEFAULT_GRACE_PERIOD);
        self.spawn_runtime(&mut runtime, &shared)?;
        Ok(runtime.info())
    }

    /// Forward keystrokes (or pasted text) to a live session's PTY.
    pub fn write(&self, session_id: &str, data: &str) -> Result<(), SessionError> {
        let shared = self.session(session_id)?;
        let mut runtime = lock_runtime(&shared)?;
        let state = runtime.state;
        if !matches!(state, ProcessState::Starting | ProcessState::Running) {
            return Err(SessionError::NotLive {
                id: session_id.to_string(),
                state,
            });
        }
        let pty = runtime.pty.as_mut().ok_or(SessionError::NotLive {
            id: session_id.to_string(),
            state,
        })?;
        pty.write(data.as_bytes())?;
        Ok(())
    }

    /// Resize a live session's PTY so full-screen TUIs reflow (spec section 11).
    pub fn resize(&self, session_id: &str, cols: u32, rows: u32) -> Result<(), SessionError> {
        let shared = self.session(session_id)?;
        let mut runtime = lock_runtime(&shared)?;
        let size = TerminalSize::from_columns_rows(cols, rows);
        let state = runtime.state;
        let pty = runtime.pty.as_mut().ok_or(SessionError::NotLive {
            id: session_id.to_string(),
            state,
        })?;
        pty.resize(size)?;
        runtime.size = size;
        Ok(())
    }

    /// One session's current state.
    pub fn get(&self, session_id: &str) -> Result<SessionInfo, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(runtime.info())
    }

    /// Every session, newest last, including finished ones (so the UI can still
    /// show "exited with code 1" after a crash).
    pub fn list(&self) -> Result<Vec<SessionInfo>, SessionError> {
        let snapshot = self.snapshot()?;
        let mut sessions: Vec<(u64, SessionInfo)> = Vec::with_capacity(snapshot.len());
        for shared in snapshot {
            let runtime = lock_runtime(&shared)?;
            sessions.push((runtime.sequence, runtime.info()));
        }
        sessions.sort_by_key(|(sequence, _info)| *sequence);
        Ok(sessions.into_iter().map(|(_sequence, info)| info).collect())
    }

    /// Number of sessions known to the manager.
    pub fn len(&self) -> usize {
        self.sessions.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// Whether the manager knows of no sessions.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The per-session configuration directory for `session_id`.
    pub fn config_directory(&self, session_id: &str) -> Result<PathBuf, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(runtime.config_directory.clone())
    }

    /// Stop every session that belongs to `workspace_id`, returning how many
    /// were stopped (spec section 15: no agent process is left behind when its
    /// workspace is removed from the application).
    ///
    /// Used by `delete_workspace`. Sessions of other workspaces are untouched,
    /// which is the same per-session isolation [`SessionManager::stop`] keeps.
    pub fn stop_workspace(
        &self,
        workspace_id: &str,
        grace: Duration,
    ) -> Result<usize, SessionError> {
        let mut stopped = 0;
        for shared in self.snapshot()? {
            let mut runtime = match shared.lock() {
                Ok(runtime) => runtime,
                Err(_) => continue,
            };
            if runtime.workspace_id != workspace_id || runtime.state.is_terminal() {
                continue;
            }
            self.stop_runtime(&mut runtime, grace);
            stopped += 1;
        }
        Ok(stopped)
    }

    /// Stop every session: graceful first, then forced (spec section 15).
    ///
    /// Called from the Tauri run loop when the application exits, so no agent
    /// process can be orphaned. The whole call is bounded by `grace` plus a
    /// short forced-termination window per session, and it never panics: an
    /// application that is closing must still close.
    pub fn stop_all(&self, grace: Duration) {
        let snapshot = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log::warn!("could not stop sessions on exit: {error}");
                return;
            }
        };
        let deadline = Instant::now() + grace;
        for shared in snapshot {
            let Ok(mut runtime) = shared.lock() else {
                continue;
            };
            // Sessions wind down in sequence but inside one shared grace
            // window, so the total wait does not grow with the session count.
            let remaining = deadline.saturating_duration_since(Instant::now());
            self.stop_runtime(&mut runtime, remaining);
        }
    }

    /// Test-only: the environment a session was built with.
    ///
    /// Contains secrets, which is exactly why it is not part of the production
    /// surface (spec section 17). It exists so the isolation tests can prove
    /// that two sessions really get different provider environments.
    #[cfg(test)]
    pub(crate) fn environment_for_testing(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String)>, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(runtime
            .spawn
            .as_ref()
            .map(|spawn| spawn.environment.clone())
            .unwrap_or_default())
    }

    /// Test-only: the working directory a session was built with.
    #[cfg(test)]
    pub(crate) fn working_directory_for_testing(
        &self,
        session_id: &str,
    ) -> Result<PathBuf, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(runtime
            .spawn
            .as_ref()
            .map(|spawn| spawn.cwd.clone())
            .unwrap_or_else(|| runtime.project_path.clone()))
    }

    /// Test-only: whether the session's own child process is still alive.
    ///
    /// This is how the lifecycle tests assert "no agent process is left behind"
    /// for the processes *this test started* - never by enumerating processes,
    /// which would see the user's own `claude.exe` instances (spec section 15).
    #[cfg(test)]
    pub(crate) fn process_is_alive_for_testing(&self, session_id: &str) -> Result<bool, SessionError> {
        let shared = self.session(session_id)?;
        let mut runtime = lock_runtime(&shared)?;
        match runtime.pty.as_mut() {
            Some(pty) => Ok(pty.is_alive()?),
            None => Ok(false),
        }
    }

    /// Test-only: the platform process id of the current run, when there is one.
    #[cfg(test)]
    pub(crate) fn process_id_for_testing(&self, session_id: &str) -> Result<Option<u32>, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(runtime.pty.as_ref().and_then(PtyProcess::process_id))
    }

    /// Test-only: the session runtime's `Debug` rendering.
    ///
    /// The runtime holds a credential-bearing environment, so its `Debug` must
    /// stay redacted; this exposes the rendering so a test can assert it (spec
    /// section 17).
    #[cfg(test)]
    pub(crate) fn runtime_debug_for_testing(&self, session_id: &str) -> Result<String, SessionError> {
        let shared = self.session(session_id)?;
        let runtime = lock_runtime(&shared)?;
        Ok(format!("{runtime:?}"))
    }

    // --- internals ----------------------------------------------------------

    /// Build the spawn description and put a real process behind it.
    fn spawn_runtime(
        &self,
        runtime: &mut SessionRuntime,
        shared: &Arc<Mutex<SessionRuntime>>,
    ) -> Result<(), SessionError> {
        // Created | Stopped | Failed -> Starting (spec section 15). A refused
        // transition means the caller's view of the session is wrong, and that
        // must surface rather than silently doing something else.
        runtime.state = runtime.state.transition(ProcessState::Starting)?;
        notify_state(runtime, self.listener.as_ref());

        match self.build_and_spawn(runtime, shared) {
            Ok(()) => {
                runtime.state = runtime
                    .state
                    .transition(ProcessState::Running)
                    .expect("Starting -> Running is always allowed");
                notify_state(runtime, self.listener.as_ref());
                Ok(())
            }
            Err(error) => {
                // Spec section 15: Starting -> Failed.
                if let Ok(failed) = runtime.state.transition(ProcessState::Failed) {
                    runtime.state = failed;
                    notify_state(runtime, self.listener.as_ref());
                }
                Err(error)
            }
        }
    }

    /// The part of a start that can fail: description, directory, PTY, supervisor.
    fn build_and_spawn(
        &self,
        runtime: &mut SessionRuntime,
        shared: &Arc<Mutex<SessionRuntime>>,
    ) -> Result<(), SessionError> {
        let spawn = runtime.adapter.spawn_description(&SpawnRequest {
            project_path: runtime.project_path.clone(),
            config_dir: runtime.config_directory.clone(),
            provider: runtime.provider.clone(),
        })?;

        // The adapter creates it too; doing it here as well means a session
        // always has a directory of its own even for an adapter that does not
        // know about `CLAUDE_CONFIG_DIR` (spec section 8).
        std::fs::create_dir_all(&runtime.config_directory).map_err(|error| {
            SessionError::SessionDirectory {
                path: runtime.config_directory.display().to_string(),
                message: error.to_string(),
            }
        })?;

        let pty = PtyProcess::spawn(
            &spawn,
            runtime.size,
            output_callback(Arc::clone(&self.listener), runtime.id.clone()),
        )?;

        runtime.spawn = Some(spawn);
        runtime.exit = None;
        runtime.pty = Some(pty);

        // One supervisor per run. A restart can briefly overlap with the
        // previous supervisor; that is harmless because the supervisor re-reads
        // the runtime every iteration and every transition goes through the
        // state machine, which rejects anything illegal.
        let supervisor_shared = Arc::clone(shared);
        let supervisor_listener = Arc::clone(&self.listener);
        thread::Builder::new()
            .name(format!("session-supervisor-{}", runtime.id))
            .spawn(move || supervisor_loop(supervisor_shared, supervisor_listener))
            .map_err(|error| SessionError::Supervisor(error.to_string()))?;

        // Orphan protection is part of what a start reports: if the OS-level
        // backstop could not be armed (see `process::guard`), that must be visible
        // in the log next to the pid it should have covered rather than only in a
        // passing warning. It never fails a start (spec section 16).
        let orphan_protection = match runtime.pty.as_ref() {
            Some(pty) if pty.is_orphan_protected() => "armed",
            _ => "unavailable",
        };
        log::info!(
            "session {} started: {} in {} (pid {:?}, orphan protection {})",
            runtime.id,
            runtime.agent_name,
            runtime.project_path.display(),
            runtime.pty.as_ref().and_then(PtyProcess::process_id),
            orphan_protection
        );
        Ok(())
    }

    /// Graceful stop, then forced, then the terminal state (spec section 15).
    fn stop_runtime(&self, runtime: &mut SessionRuntime, grace: Duration) {
        if runtime.state.is_terminal() {
            return;
        }
        if let Ok(stopping) = runtime.state.transition(ProcessState::Stopping) {
            runtime.state = stopping;
            notify_state(runtime, self.listener.as_ref());
        }

        if let Some(exit) = terminate(runtime, grace) {
            runtime.exit = Some(exit);
        }

        // A session the user stopped reports `Stopped` even if the process had
        // to be killed (a non-zero code from a forced stop is expected, not a
        // failure); only a process that could not be reaped at all is `Failed`.
        let next = if runtime.exit.is_some() {
            ProcessState::Stopped
        } else {
            ProcessState::Failed
        };
        if let Ok(state) = runtime.state.transition(next) {
            runtime.state = state;
            notify_state(runtime, self.listener.as_ref());
        }
    }

    /// The session for `session_id`, if it is known.
    fn session(&self, session_id: &str) -> Result<Arc<Mutex<SessionRuntime>>, SessionError> {
        self.lock_registry()?
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    /// Registry snapshot; the lock is released before any session is touched.
    fn snapshot(&self) -> Result<Vec<Arc<Mutex<SessionRuntime>>>, SessionError> {
        Ok(self.lock_registry()?.values().cloned().collect())
    }

    /// The live session of `workspace_id`, if any (there is at most one).
    fn live_session_for(&self, workspace_id: &str) -> Result<Option<SessionInfo>, SessionError> {
        let mut live: Vec<(u64, SessionInfo)> = Vec::new();
        for shared in self.snapshot()? {
            let runtime = lock_runtime(&shared)?;
            if runtime.workspace_id == workspace_id && !runtime.state.is_terminal() {
                live.push((runtime.sequence, runtime.info()));
            }
        }
        live.sort_by_key(|(sequence, _info)| *sequence);
        Ok(live.into_iter().next().map(|(_sequence, info)| info))
    }

    /// Release finished sessions of one workspace (their PTYs included) so a
    /// restart does not accumulate stopped processes and pseudo-consoles.
    fn remove_finished_sessions(&self, workspace_id: &str) -> Result<(), SessionError> {
        let mut registry = self.lock_registry()?;
        registry.retain(|_id, shared| match shared.lock() {
            Ok(runtime) => !(runtime.workspace_id == workspace_id && runtime.state.is_terminal()),
            // A poisoned lock means we cannot prove the session is finished;
            // keeping it is the safe choice.
            Err(_) => true,
        });
        Ok(())
    }

    fn lock_registry(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<SessionId, Arc<Mutex<SessionRuntime>>>>, SessionError> {
        self.sessions.lock().map_err(|_| SessionError::LockPoisoned)
    }
}

impl fmt::Debug for SessionManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionManager")
            .field("sessions_directory", &self.sessions_directory)
            .field("sessions", &self.len())
            .finish_non_exhaustive()
    }
}

/// Output callback handed to the PTY reader thread.
///
/// It captures only the session id and the listener, so output never has to
/// take a session lock to be delivered (a session being stopped must still be
/// able to flush its last lines).
fn output_callback(
    listener: Arc<dyn SessionListener>,
    session_id: SessionId,
) -> impl Fn(&[u8]) + Send + 'static {
    move |chunk: &[u8]| listener.on_output(&session_id, chunk)
}

/// Broadcast the current state.
fn notify_state(runtime: &SessionRuntime, listener: &dyn SessionListener) {
    listener.on_state(&runtime.state_event());
}

/// Poll one session's child until it exits or the session becomes terminal.
///
/// This is what turns an *unexpected* termination into a state change, so the
/// UI never shows "running" for a process that is gone (spec section 15).
fn supervisor_loop(shared: Arc<Mutex<SessionRuntime>>, listener: Arc<dyn SessionListener>) {
    loop {
        thread::sleep(EXIT_POLL_INTERVAL);
        let mut runtime = match shared.lock() {
            Ok(runtime) => runtime,
            Err(_) => return,
        };
        if runtime.state.is_terminal() {
            return;
        }
        let observed = match runtime.pty.as_mut() {
            Some(pty) => pty.try_wait(),
            // Nothing to supervise (nothing spawned yet, or already released).
            None => return,
        };
        match observed {
            Ok(Some(exit)) => {
                let next = if runtime.state == ProcessState::Stopping {
                    // A stop is in progress: its outcome, not the exit code,
                    // decides the state.
                    ProcessState::Stopped
                } else {
                    exit.state_for_unexpected_exit()
                };
                // A process that ended without being asked to is a lifecycle
                // event the user can only act on if it is diagnosable, and the
                // exit description is secret-free by construction: it names a
                // status, never anything the session was handed (spec sections
                // 15, 16, 17).
                if next == ProcessState::Failed {
                    log::warn!(
                        "session {}: {} died unexpectedly ({}) - the session is failed and can be restarted",
                        runtime.id,
                        runtime.agent_name,
                        exit.description()
                    );
                } else {
                    log::info!(
                        "session {}: {} ended ({})",
                        runtime.id,
                        runtime.agent_name,
                        exit.description()
                    );
                }
                runtime.exit = Some(exit);
                if let Ok(state) = runtime.state.transition(next) {
                    runtime.state = state;
                    notify_state(&runtime, listener.as_ref());
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                // Keep supervising: a transient polling error must not end the
                // session's supervision while the process may still be alive.
                log::debug!("session {}: could not poll the agent process: {error}", runtime.id);
            }
        }
    }
}

/// Graceful stop first, then forced termination (spec section 15).
///
/// Returns the exit status once the process is gone, or `None` if it could not
/// be reaped at all (which the caller reports as a failed stop).
fn terminate(runtime: &mut SessionRuntime, grace: Duration) -> Option<ExitInfo> {
    if let Some(exit) = runtime.exit.clone() {
        return Some(exit);
    }
    let pty = runtime.pty.as_mut()?;
    if let Ok(Some(exit)) = pty.try_wait() {
        return Some(exit);
    }

    // Graceful: interrupt the interactive agent, then close its input so a
    // well-behaved CLI sees EOF and exits.
    let _ = pty.interrupt();
    pty.close_input();
    if let Ok(Some(exit)) = pty.wait_for_exit(grace) {
        return Some(exit);
    }

    log::info!(
        "session {} did not stop within {grace:?}; forcing termination",
        runtime.id
    );
    if let Err(error) = pty.kill() {
        log::warn!("session {}: could not force-stop the agent: {error}", runtime.id);
    }
    match pty.wait_for_exit(FORCED_TERMINATION_TIMEOUT) {
        Ok(exit) => exit,
        Err(error) => {
            log::warn!("session {}: could not reap the agent: {error}", runtime.id);
            None
        }
    }
}

/// Lock one session's runtime, mapping lock poisoning to an error.
fn lock_runtime(
    shared: &Arc<Mutex<SessionRuntime>>,
) -> Result<MutexGuard<'_, SessionRuntime>, SessionError> {
    shared.lock().map_err(|_| SessionError::LockPoisoned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testing::FakeAgentAdapter;
    use crate::persistence::test_support::TempDir;
    use crate::providers::ProviderProfile;

    /// Bound for every wait in these tests, so a broken session fails the suite
    /// instead of hanging it.
    const TIMEOUT: Duration = Duration::from_secs(15);

    /// A stop grace short enough to keep the suite quick.
    const SHORT_GRACE: Duration = Duration::from_millis(200);

    /// Records everything a session reports, keyed by session id.
    #[derive(Default)]
    struct RecordingListener {
        output: Mutex<HashMap<String, String>>,
        states: Mutex<Vec<SessionStateEvent>>,
    }

    impl RecordingListener {
        fn output_for(&self, session_id: &str) -> String {
            self.output
                .lock()
                .unwrap()
                .get(session_id)
                .cloned()
                .unwrap_or_default()
        }

        fn last_state_for(&self, session_id: &str) -> Option<ProcessState> {
            self.states
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|event| event.session_id == session_id)
                .map(|event| event.status)
        }

        fn states_for(&self, session_id: &str) -> Vec<ProcessState> {
            self.states
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.session_id == session_id)
                .map(|event| event.status)
                .collect()
        }
    }

    impl SessionListener for RecordingListener {
        fn on_output(&self, session_id: &SessionId, chunk: &[u8]) {
            let mut output = self.output.lock().unwrap();
            let entry = output.entry(session_id.as_str().to_string()).or_default();
            // Same incremental decode the Tauri listener uses.
            let mut pending = Vec::new();
            entry.push_str(&decode_output_chunk(&mut pending, chunk));
        }

        fn on_state(&self, event: &SessionStateEvent) {
            self.states.lock().unwrap().push(event.clone());
        }
    }

    fn provider(id: &str, base_url: &str, model: &str, api_key: &str) -> ResolvedProvider {
        provider_with_context_window(id, base_url, model, api_key, None)
    }

    /// The same, with the model's declared context window (the `glm-5.3` case,
    /// where Claude Code's catalog does not describe the model).
    fn provider_with_context_window(
        id: &str,
        base_url: &str,
        model: &str,
        api_key: &str,
        max_context_tokens: Option<u32>,
    ) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderProfile {
                id: id.to_string(),
                name: id.to_string(),
                base_url: base_url.to_string(),
                model: model.to_string(),
                extra_env: Vec::new(),
                max_context_tokens,
            },
            Some(api_key.to_string()),
        )
    }

    struct Fixture {
        manager: SessionManager,
        listener: Arc<RecordingListener>,
        _root: TempDir,
        project: TempDir,
    }

    fn fixture(label: &str) -> Fixture {
        let root = TempDir::new(label);
        let project = TempDir::new(&format!("{label}-project"));
        let listener = Arc::new(RecordingListener::default());
        let manager = SessionManager::new(
            root.join(SESSIONS_DIRECTORY_NAME),
            Arc::clone(&listener) as Arc<dyn SessionListener>,
        );
        Fixture {
            manager,
            listener,
            _root: root,
            project,
        }
    }

    impl Fixture {
        fn start(
            &self,
            workspace_id: &str,
            provider: ResolvedProvider,
        ) -> Result<SessionInfo, SessionError> {
            self.start_with(
                workspace_id,
                provider,
                Box::new(FakeAgentAdapter::interactive_shell()),
            )
        }

        /// Start a session with an explicit agent, so a case that needs a
        /// different process (a crash, a missing program) can use one.
        fn start_with(
            &self,
            workspace_id: &str,
            provider: ResolvedProvider,
            adapter: Box<dyn AgentAdapter>,
        ) -> Result<SessionInfo, SessionError> {
            self.manager.start(StartSessionRequest {
                workspace_id: workspace_id.to_string(),
                project_path: self.project.path().to_path_buf(),
                provider,
                adapter,
            })
        }

        /// Wait until `predicate` holds, or fail the test after [`TIMEOUT`].
        fn wait_until(&self, label: &str, predicate: impl Fn() -> bool) {
            let deadline = Instant::now() + TIMEOUT;
            while Instant::now() < deadline {
                if predicate() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            panic!("timed out waiting for {label}");
        }

        /// An interactive shell needs its input before it produces anything on
        /// Windows (`ConPTY` asks the terminal for the cursor position first),
        /// so the test plays terminal for one handshake.
        fn answer_cursor_query(&self, session_id: &str) {
            let _ = self.manager.write(session_id, "\u{1b}[1;1R");
        }

        /// Answer `ConPTY`'s opening cursor-position query.
        ///
        /// Windows `ConPTY` asks the attached terminal where the cursor is and
        /// holds the session's output - including the child's own console
        /// writes - until it gets an answer. Without a terminal emulator in the
        /// loop a test must answer it itself; `xterm.js` does this in the app.
        fn wait_for_cursor_query(&self, session_id: &str) {
            let listening = Arc::clone(&self.listener);
            let id = session_id.to_string();
            let deadline = Instant::now() + TIMEOUT;
            while Instant::now() < deadline {
                if listening.output_for(&id).contains("\u{1b}[6n") {
                    self.answer_cursor_query(&id);
                    // Give the reply a moment to be consumed before asserting on
                    // what happens next.
                    thread::sleep(Duration::from_millis(25));
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            panic!("timed out waiting for the terminal handshake on session {id}");
        }

        /// Type a command into a session and wait for its output to come back.
        fn expect_echo(&self, session_id: &str, token: &str) {
            let deadline = Instant::now() + TIMEOUT;
            let mut sent = false;
            while Instant::now() < deadline {
                if self.listener.output_for(session_id).contains(token) {
                    return;
                }
                if !sent && self.listener.output_for(session_id).contains("\u{1b}[6n") {
                    self.answer_cursor_query(session_id);
                    if self
                        .manager
                        .write(session_id, &format!("echo {token}\r\n"))
                        .is_ok()
                    {
                        sent = true;
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
            panic!(
                "timed out waiting for `{token}`; output was {:?}",
                self.listener.output_for(session_id)
            );
        }
    }

    #[test]
    fn decode_output_chunk_keeps_split_utf8_characters_whole() {
        // A box-drawing character arriving in two reads must not turn into two
        // replacement characters.
        let character = "─"; // U+2500, three bytes
        let bytes = character.as_bytes();

        let mut pending = Vec::new();
        assert_eq!(decode_output_chunk(&mut pending, &bytes[..1]), "");
        assert_eq!(decode_output_chunk(&mut pending, &bytes[1..]), character);
        assert!(pending.is_empty(), "pending must be drained: {pending:?}");

        // Truly invalid bytes are replaced, and decoding continues.
        let mut pending = Vec::new();
        let mut invalid = vec![b'a', 0xFF, 0xFE, b'b'];
        invalid.extend_from_slice("é".as_bytes());
        assert_eq!(decode_output_chunk(&mut pending, &invalid), "a\u{FFFD}\u{FFFD}bé");

        // A multi-byte character split across three chunks.
        let mut pending = Vec::new();
        assert_eq!(decode_output_chunk(&mut pending, &bytes[..1]), "");
        assert_eq!(decode_output_chunk(&mut pending, &bytes[1..2]), "");
        assert_eq!(decode_output_chunk(&mut pending, &bytes[2..]), character);
    }

    #[test]
    fn a_session_starts_runs_accepts_input_and_stops() {
        let fixture = fixture("session-lifecycle");
        let info = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .expect("start a session");

        assert_eq!(info.workspace_id, "workspace-a");
        assert_eq!(info.status, ProcessState::Running);
        assert_eq!(info.exit_code, None);
        // The PTY starts at the default geometry until the terminal reports its
        // own (the frontend resizes on the transition to running).
        assert_eq!((info.cols, info.rows), (80, 24));
        assert!(!info.id.is_empty());

        let session_id = info.id.clone();
        assert_eq!(fixture.manager.get(&session_id).unwrap().status, ProcessState::Running);
        assert_eq!(fixture.manager.list().unwrap().len(), 1);

        // Typing into the PTY reaches the process and its output comes back
        // through the session's own channel.
        let token = format!("SESSION-ECHO-{}", std::process::id());
        fixture.expect_echo(&session_id, &token);

        // A resize is forwarded to the PTY and reported back.
        fixture
            .manager
            .resize(&session_id, 132, 43)
            .expect("resize a live session");
        let resized = fixture.manager.get(&session_id).unwrap();
        assert_eq!((resized.cols, resized.rows), (132, 43));

        // Stop: graceful first, then forced. Either way the session ends
        // `Stopped` and the process is gone.
        fixture
            .manager
            .stop_with_grace(&session_id, SHORT_GRACE)
            .expect("stop the session");

        let stopped = fixture.manager.get(&session_id).unwrap();
        assert_eq!(stopped.status, ProcessState::Stopped);
        assert_ne!(stopped.exit_code, None, "a stopped process reports an exit code");
        assert_eq!(fixture.listener.last_state_for(&session_id), Some(ProcessState::Stopped));

        // The lifecycle was reported in order, and `created` was never skipped.
        let states = fixture.listener.states_for(&session_id);
        assert_eq!(
            states,
            vec![
                ProcessState::Starting,
                ProcessState::Running,
                ProcessState::Stopping,
                ProcessState::Stopped,
            ]
        );

        // Stopping again is a no-op, and writing to a stopped session is an error.
        fixture
            .manager
            .stop_with_grace(&session_id, SHORT_GRACE)
            .expect("stopping a stopped session is harmless");
        assert!(matches!(
            fixture.manager.write(&session_id, "x"),
            Err(SessionError::NotLive { .. })
        ));
    }

    #[test]
    fn stopping_one_session_does_not_touch_another() {
        let fixture = fixture("session-isolation-stop");
        let session_a = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        let session_b = fixture
            .start("workspace-b", provider("provider-b", "https://b.example.com", "model-b", "KEY_B"))
            .unwrap();

        assert_ne!(session_a.id, session_b.id);
        assert_eq!(fixture.manager.list().unwrap().len(), 2);

        // Both are alive; stop A only.
        fixture
            .manager
            .stop_with_grace(&session_a.id, SHORT_GRACE)
            .expect("stop session A");

        let stopped_a = fixture.manager.get(&session_a.id).unwrap();
        let running_b = fixture.manager.get(&session_b.id).unwrap();
        assert_eq!(stopped_a.status, ProcessState::Stopped);
        assert_eq!(
            running_b.status,
            ProcessState::Running,
            "stopping session A must not stop session B"
        );

        // B is still fully functional: input still round-trips through it.
        let token = format!("STILL-ALIVE-{}", std::process::id());
        fixture.expect_echo(&session_b.id, &token);
        // ... and none of B's output leaked into A's channel.
        assert!(!fixture.listener.output_for(&session_a.id).contains(&token));
        // A's earlier output did not leak into B either.
        assert!(!fixture.listener.output_for(&session_b.id).contains("SESSION-ECHO"));

        // Every session is stopped at the end of the test's own lifetime.
        fixture
            .manager
            .stop_with_grace(&session_b.id, SHORT_GRACE)
            .expect("stop session B");
        assert_eq!(
            fixture.manager.get(&session_b.id).unwrap().status,
            ProcessState::Stopped
        );
    }

    #[test]
    fn stopping_a_workspace_stops_only_its_own_sessions() {
        // Spec section 15: removing a workspace from the application must not
        // leave its agent process behind - and must not touch anyone else's.
        let fixture = fixture("session-stop-workspace");
        let session_a = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        let session_b = fixture
            .start("workspace-b", provider("provider-b", "https://b.example.com", "model-b", "KEY_B"))
            .unwrap();

        assert_eq!(
            fixture.manager.stop_workspace("workspace-a", SHORT_GRACE).unwrap(),
            1
        );
        assert_eq!(fixture.manager.get(&session_a.id).unwrap().status, ProcessState::Stopped);
        assert_eq!(
            fixture.manager.get(&session_b.id).unwrap().status,
            ProcessState::Running,
            "a workspace stop must not reach another workspace's session"
        );

        // Idempotent, and an unknown workspace is not an error.
        assert_eq!(
            fixture.manager.stop_workspace("workspace-a", SHORT_GRACE).unwrap(),
            0
        );
        assert_eq!(
            fixture.manager.stop_workspace("no-such-workspace", SHORT_GRACE).unwrap(),
            0
        );

        fixture.manager.stop_all(SHORT_GRACE);
    }

    #[test]
    fn sessions_get_their_own_environment_and_never_touch_the_app_environment() {
        // Spec sections 7, 8: the core isolation requirement, at the session
        // level: two sessions, two providers, one untouched application.
        let fixture = fixture("session-environment-isolation");

        let before = process_environment_snapshot();

        let session_a = fixture
            .start(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", "KEY_A"),
            )
            .unwrap();
        let session_b = fixture
            .start(
                "workspace-b",
                provider("provider-b", "https://b.example.com", "model-b", "KEY_B"),
            )
            .unwrap();

        let environment_a = fixture.manager.environment_for_testing(&session_a.id).unwrap();
        let environment_b = fixture.manager.environment_for_testing(&session_b.id).unwrap();

        let value = |environment: &[(String, String)], name: &str| {
            environment
                .iter()
                .filter(|(key, _value)| key == name)
                .next_back()
                .map(|(_key, value)| value.clone())
        };

        assert_eq!(
            value(&environment_a, "ANTHROPIC_BASE_URL"),
            Some("https://a.example.com".to_string())
        );
        assert_eq!(
            value(&environment_b, "ANTHROPIC_BASE_URL"),
            Some("https://b.example.com".to_string())
        );
        assert_eq!(value(&environment_a, "ANTHROPIC_AUTH_TOKEN"), Some("KEY_A".to_string()));
        assert_eq!(value(&environment_b, "ANTHROPIC_AUTH_TOKEN"), Some("KEY_B".to_string()));
        // Both sessions authenticate as bearer tokens and neither is given an
        // API-key header, so no gateway sees two credential styles at once
        // (spec section 5).
        assert_eq!(value(&environment_a, "ANTHROPIC_API_KEY"), None);
        assert_eq!(value(&environment_b, "ANTHROPIC_API_KEY"), None);
        assert_eq!(value(&environment_a, "ANTHROPIC_MODEL"), Some("model-a".to_string()));
        assert_eq!(value(&environment_b, "ANTHROPIC_MODEL"), Some("model-b".to_string()));
        assert_ne!(environment_a, environment_b);
        // No value from A leaked into B.
        assert!(!environment_b.iter().any(|(_key, value)| value == "KEY_A"));

        // Configuration isolation: one directory per session (spec section 8).
        let config_a = fixture.manager.config_directory(&session_a.id).unwrap();
        let config_b = fixture.manager.config_directory(&session_b.id).unwrap();
        assert_ne!(config_a, config_b);
        assert!(config_a.is_dir() && config_b.is_dir());
        assert_eq!(
            value(&environment_a, "CLAUDE_CONFIG_DIR"),
            Some(config_a.display().to_string())
        );
        assert_eq!(
            value(&environment_b, "CLAUDE_CONFIG_DIR"),
            Some(config_b.display().to_string())
        );

        // Both sessions run in the workspace's project folder, not in the
        // application's own working directory.
        assert_eq!(
            fixture.manager.working_directory_for_testing(&session_a.id).unwrap(),
            fixture.project.path()
        );
        assert_eq!(
            fixture.manager.working_directory_for_testing(&session_b.id).unwrap(),
            fixture.project.path()
        );

        // The application's own environment is untouched (spec section 7: never
        // modify the user's global environment).
        assert_eq!(before, process_environment_snapshot());

        fixture.manager.stop_with_grace(&session_a.id, SHORT_GRACE).ok();
        fixture.manager.stop_with_grace(&session_b.id, SHORT_GRACE).ok();
    }

    /// The `glm-5.3` case end to end, one layer above the adapter (FIX 2): a
    /// provider profile with a declared context window produces a session whose
    /// environment tells Claude Code the real window, and a profile without one
    /// leaves the variable out entirely so Claude Code's own fallback applies.
    #[test]
    fn a_session_declares_its_providers_context_window_and_its_credential_as_a_bearer_token() {
        let fixture = fixture("session-context-window");

        let declared = fixture
            .start(
                "workspace-a",
                provider_with_context_window(
                    "provider-a",
                    "https://a.example.com",
                    "glm-5.3",
                    "KEY_A",
                    Some(1_000_000),
                ),
            )
            .unwrap();
        let declared_environment = fixture.manager.environment_for_testing(&declared.id).unwrap();
        let value = |name: &str| {
            declared_environment
                .iter()
                .filter(|(key, _value)| key == name)
                .next_back()
                .map(|(_key, value)| value.clone())
        };

        assert_eq!(
            value("CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            Some("1000000".to_string())
        );
        assert_eq!(value("ANTHROPIC_AUTH_TOKEN"), Some("KEY_A".to_string()));
        assert_eq!(value("ANTHROPIC_API_KEY"), None);

        let undeclared = fixture
            .start(
                "workspace-b",
                provider("provider-b", "https://b.example.com", "model-b", "KEY_B"),
            )
            .unwrap();
        let undeclared_environment = fixture.manager.environment_for_testing(&undeclared.id).unwrap();
        assert!(
            !undeclared_environment
                .iter()
                .any(|(name, _value)| name == "CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            "an undeclared window must not produce the variable: {undeclared_environment:?}"
        );

        fixture.manager.stop_with_grace(&declared.id, SHORT_GRACE).ok();
        fixture.manager.stop_with_grace(&undeclared.id, SHORT_GRACE).ok();
    }

    #[test]
    fn restarting_a_session_keeps_its_id_and_starts_a_fresh_process() {
        let fixture = fixture("session-restart");
        let info = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();

        let restarted = fixture.manager.restart(&info.id).expect("restart the session");
        assert_eq!(restarted.id, info.id, "the frontend keeps addressing one id");
        assert_eq!(restarted.status, ProcessState::Running);
        // A fresh process: the previous one is gone and its exit is forgotten.
        assert_eq!(restarted.exit_code, None);
        assert_eq!(
            fixture.listener.states_for(&info.id),
            vec![
                ProcessState::Starting,
                ProcessState::Running,
                ProcessState::Stopping,
                ProcessState::Stopped,
                ProcessState::Starting,
                ProcessState::Running,
            ]
        );

        // The restarted session is usable.
        let token = format!("RESTARTED-{}", std::process::id());
        fixture.expect_echo(&info.id, &token);

        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).ok();
    }

    #[test]
    fn restarting_a_finished_session_works_and_stopping_it_twice_is_harmless() {
        // The frontend clears its session id on stop, but a stale tab can still
        // ask for a restart: `Stopped -> Starting` must be allowed (the
        // documented restart extension of the state machine).
        let fixture = fixture("session-restart-stopped");
        let info = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).unwrap();
        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).unwrap();

        let restarted = fixture.manager.restart(&info.id).unwrap();
        assert_eq!(restarted.status, ProcessState::Running);
        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).ok();
    }

    #[test]
    fn starting_a_workspace_twice_reuses_its_live_session() {
        // A UI reload loses the session id; `start_session` must not spawn a
        // second agent for the same workspace.
        let fixture = fixture("session-reattach");
        let first = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        let second = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(fixture.manager.list().unwrap().len(), 1);

        // Once the session has finished, a new start replaces it.
        fixture.manager.stop_with_grace(&first.id, SHORT_GRACE).unwrap();
        let third = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        assert_ne!(third.id, first.id);
        assert_eq!(fixture.manager.list().unwrap().len(), 1);

        fixture.manager.stop_with_grace(&third.id, SHORT_GRACE).ok();
    }

    #[test]
    fn a_session_whose_process_exits_is_reported_as_finished() {
        // Unexpected termination (spec section 15): the child exits on its own,
        // with no stop request, and the state must follow.
        let fixture = fixture("session-unexpected-exit");
        let token = format!("EXIT-{}", std::process::id());

        let info = fixture
            .manager
            .start(StartSessionRequest {
                workspace_id: "workspace-a".to_string(),
                project_path: fixture.project.path().to_path_buf(),
                provider: provider("provider-a", "https://a.example.com", "model-a", "KEY_A"),
                adapter: Box::new(FakeAgentAdapter::echo(&token)),
            })
            .expect("start a short-lived session");

        // The short-lived child only gets to run once the terminal handshake is
        // answered (see `wait_for_cursor_query`).
        fixture.wait_for_cursor_query(&info.id);

        fixture.wait_until("the session to report its exit", || {
            matches!(
                fixture.manager.get(&info.id).map(|session| session.status),
                Ok(ProcessState::Stopped) | Ok(ProcessState::Failed)
            )
        });

        let finished = fixture.manager.get(&info.id).unwrap();
        assert_eq!(finished.exit_code, Some(0), "a clean exit is not a failure");
        assert!(fixture.listener.states_for(&info.id).contains(&ProcessState::Stopped));
        // Its output was still delivered, even though the process is gone.
        assert!(
            fixture.listener.output_for(&info.id).contains(&token),
            "the finished session's output was lost: {:?}",
            fixture.listener.output_for(&info.id)
        );
    }

    #[test]
    fn unknown_sessions_are_reported_clearly() {
        let fixture = fixture("session-unknown");
        let missing = "no-such-session";

        assert!(matches!(
            fixture.manager.get(missing),
            Err(SessionError::NotFound(id)) if id == missing
        ));
        assert!(matches!(
            fixture.manager.stop(missing),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            fixture.manager.restart(missing),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            fixture.manager.write(missing, "x"),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            fixture.manager.resize(missing, 80, 24),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            fixture.manager.config_directory(missing),
            Err(SessionError::NotFound(_))
        ));
        assert!(fixture.manager.list().unwrap().is_empty());
        assert!(fixture.manager.is_empty());
    }

    #[test]
    fn a_missing_project_folder_fails_the_start_and_leaves_no_session_behind() {
        let fixture = fixture("session-missing-project");
        let error = fixture
            .manager
            .start(StartSessionRequest {
                workspace_id: "workspace-a".to_string(),
                project_path: fixture.project.join("deleted"),
                provider: provider("provider-a", "https://a.example.com", "model-a", "KEY_A"),
                adapter: Box::new(FakeAgentAdapter::interactive_shell()),
            })
            .expect_err("a deleted project folder must not start a session");

        // The error is the agent's, and it is user-facing (spec section 16).
        assert!(matches!(error, SessionError::Agent(AgentError::ProjectDirectoryMissing { .. })));
        assert!(error.to_string().contains("does not exist"));
        // Nothing was registered, so the UI cannot show a phantom session.
        assert!(fixture.manager.list().unwrap().is_empty());
        assert!(!fixture.listener.states_for("workspace-a").contains(&ProcessState::Failed));
    }

    // --- lifecycle: stop, crash, restart (Milestone 5, step 3) --------------
    //
    // Spec section 15 asks for graceful shutdown first, forced termination when
    // necessary, and accurate state for an unexpected exit. These tests pin each
    // of those paths, and every one of them asserts on the process the test
    // itself started - never by enumerating processes, which would also see the
    // user's own `claude.exe` instances.

    /// A credential-looking value, used to prove that no lifecycle message or
    /// `Debug` rendering carries it (spec section 17).
    const SECRET: &str = "sk-DO-NOT-LEAK-7b2e40";

    #[test]
    fn a_session_that_stops_on_request_does_so_well_inside_the_grace_period() {
        // The graceful half of the stop (interrupt + EOF on the agent's input)
        // must be what ends a well-behaved agent. The discriminator is time: the
        // grace period is far longer than the stop needs, so an assertion that
        // the stop returned quickly fails if the process had to be waited out
        // and then killed (spec section 15).
        let fixture = fixture("session-graceful-stop");
        let info = fixture
            .start(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
            )
            .expect("start a session");

        // It is genuinely running and responsive before the stop.
        let token = format!("GRACEFUL-{}", std::process::id());
        fixture.expect_echo(&info.id, &token);
        assert!(fixture
            .manager
            .process_is_alive_for_testing(&info.id)
            .expect("poll the agent process"));

        let grace = Duration::from_secs(5);
        let started = Instant::now();
        fixture
            .manager
            .stop_with_grace(&info.id, grace)
            .expect("stop the session");
        let elapsed = started.elapsed();

        assert!(
            elapsed < grace / 2,
            "the stop took {elapsed:?} with a {grace:?} grace period, so the agent did not \
             stop on the graceful path: it was terminated forcibly after the grace period"
        );
        let stopped = fixture.manager.get(&info.id).unwrap();
        assert_eq!(stopped.status, ProcessState::Stopped);
        assert!(stopped.exit_code.is_some(), "a stopped process reports an exit: {stopped:?}");
        assert!(
            !fixture
                .manager
                .process_is_alive_for_testing(&info.id)
                .expect("poll the agent process"),
            "no agent process may survive the stop"
        );
        assert_eq!(
            fixture.listener.states_for(&info.id),
            vec![
                ProcessState::Starting,
                ProcessState::Running,
                ProcessState::Stopping,
                ProcessState::Stopped,
            ]
        );
        // Nothing in the lifecycle messages can carry the session's credential.
        for state in fixture.listener.states_for(&info.id) {
            assert!(!format!("{state:?}").contains(SECRET));
        }
    }

    #[test]
    fn a_session_that_is_still_alive_when_the_grace_period_expires_is_terminated() {
        // The forced half of the stop (spec section 15).
        //
        // A zero-length grace period is how this branch is reached
        // deterministically: `terminate` asks the child for its exit status
        // with a zero budget, gives up immediately, and kills. With a real
        // child there is no other way to reach it on Windows - measured on this
        // machine, closing the PTY's input ends a ConPTY client within ~10 ms
        // and Ctrl-C does not end one at all, so no child survives the graceful
        // half. The child here is verified to be alive first, which is what the
        // branch is for.
        let fixture = fixture("session-forced-stop");
        let info = fixture
            .start(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
            )
            .expect("start a session");
        let token = format!("FORCED-{}", std::process::id());
        fixture.expect_echo(&info.id, &token);
        assert!(
            fixture
                .manager
                .process_is_alive_for_testing(&info.id)
                .expect("poll the agent process"),
            "the child must be alive when the grace period expires"
        );

        let started = Instant::now();
        fixture
            .manager
            .stop_with_grace(&info.id, Duration::ZERO)
            .expect("stop the session");
        assert!(
            started.elapsed() < TIMEOUT,
            "a forced stop must not hang: {:?}",
            started.elapsed()
        );

        let stopped = fixture.manager.get(&info.id).unwrap();
        assert_eq!(stopped.status, ProcessState::Stopped);
        let exit_code = stopped.exit_code.expect("a terminated process reports an exit");
        assert!(
            !fixture
                .manager
                .process_is_alive_for_testing(&info.id)
                .expect("poll the agent process"),
            "the forced path must leave no agent process"
        );
        // On Windows the forced path is `TerminateProcess`, whose status is 1 -
        // so this is positive evidence that the kill is what ended the child
        // rather than the graceful request.
        #[cfg(windows)]
        assert_eq!(
            exit_code, 1,
            "the child must have been terminated by the forced path"
        );
        // The state machine still reports a stop, not a failure: the user asked
        // for it (spec section 15).
        assert_eq!(
            fixture.listener.states_for(&info.id),
            vec![
                ProcessState::Starting,
                ProcessState::Running,
                ProcessState::Stopping,
                ProcessState::Stopped,
            ]
        );
        assert!(!stopped.exit_code.unwrap().to_string().contains(SECRET));
    }

    #[test]
    fn an_agent_that_crashes_is_reported_as_failed_with_its_exit_code() {
        // Spec section 15: unexpected termination, and spec section 16:
        // "Process crash". The process is never asked to stop - it exits with a
        // non-zero code on its own - and the session must say so.
        let fixture = fixture("session-crash");
        let info = fixture
            .start_with(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
                Box::new(FakeAgentAdapter::exit_with(7)),
            )
            .expect("start a crashing session");

        fixture.wait_for_cursor_query(&info.id);
        fixture.wait_until("the crash to be reported", || {
            matches!(
                fixture.manager.get(&info.id).map(|session| session.status),
                Ok(ProcessState::Failed)
            )
        });

        let crashed = fixture.manager.get(&info.id).unwrap();
        assert_eq!(crashed.exit_code, Some(7), "the exit code must reach the UI");
        assert!(fixture.listener.states_for(&info.id).contains(&ProcessState::Failed));
        assert!(
            !fixture.listener.states_for(&info.id).contains(&ProcessState::Stopped),
            "a crash must not be reported as a clean stop: {:?}",
            fixture.listener.states_for(&info.id)
        );

        // The process is gone, and the session is terminal: nothing may be
        // written to it until it is restarted.
        assert!(!fixture
            .manager
            .process_is_alive_for_testing(&info.id)
            .expect("poll the agent process"));
        let error = fixture.manager.write(&info.id, "x").unwrap_err();
        assert!(matches!(error, SessionError::NotLive { .. }));
        assert!(!error.to_string().contains(SECRET));
        assert!(!format!("{error:?}").contains(SECRET));
        // A stopped-crash session cannot be "stopped" into a different verdict.
        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).unwrap();
        assert_eq!(fixture.manager.get(&info.id).unwrap().status, ProcessState::Failed);
    }

    #[test]
    fn a_crashed_session_can_be_restarted_and_runs_a_working_process_again() {
        // Spec section 15: `Failed` is terminal but restartable. The adapter
        // crashes on its first process and runs the interactive shell on every
        // later one, so the restarted session can be *used* - which is the
        // difference between "the state machine allows it" and "it works".
        let fixture = fixture("session-restart-after-crash");
        let info = fixture
            .start_with(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
                Box::new(FakeAgentAdapter::crash_then_run_shell(7)),
            )
            .expect("start a session whose agent crashes");

        fixture.wait_for_cursor_query(&info.id);
        fixture.wait_until("the crash to be reported", || {
            matches!(
                fixture.manager.get(&info.id).map(|session| session.status),
                Ok(ProcessState::Failed)
            )
        });
        assert_eq!(fixture.manager.get(&info.id).unwrap().exit_code, Some(7));

        let restarted = fixture.manager.restart(&info.id).expect("restart a crashed session");
        assert_eq!(restarted.id, info.id, "the frontend keeps addressing one id");
        assert_eq!(restarted.status, ProcessState::Running);
        assert_eq!(
            restarted.exit_code, None,
            "a restart must not report the previous crash's exit code"
        );
        assert_eq!(
            fixture.listener.states_for(&info.id),
            vec![
                ProcessState::Starting,
                ProcessState::Running,
                ProcessState::Failed,
                ProcessState::Starting,
                ProcessState::Running,
            ]
        );

        // The restarted session is a working session, not just a plausible
        // state: input reaches it and its output comes back.
        let token = format!("AFTER-CRASH-{}", std::process::id());
        fixture.expect_echo(&info.id, &token);
        assert!(fixture
            .manager
            .process_is_alive_for_testing(&info.id)
            .expect("poll the agent process"));

        fixture
            .manager
            .stop_with_grace(&info.id, SHORT_GRACE)
            .expect("stop the restarted session");
        assert_eq!(fixture.manager.get(&info.id).unwrap().status, ProcessState::Stopped);
    }

    #[test]
    fn a_restart_after_a_clean_stop_runs_a_new_process() {
        let fixture = fixture("session-restart-new-process");
        let info = fixture
            .start(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
            )
            .expect("start a session");
        let first_pid = fixture
            .manager
            .process_id_for_testing(&info.id)
            .expect("read the process id")
            .expect("the platform reports a process id");

        fixture
            .manager
            .stop_with_grace(&info.id, SHORT_GRACE)
            .expect("stop the session");
        assert!(
            !fixture
                .manager
                .process_is_alive_for_testing(&info.id)
                .expect("poll the agent process"),
            "the stopped process must be gone before the restart"
        );

        let restarted = fixture.manager.restart(&info.id).expect("restart the session");
        let second_pid = fixture
            .manager
            .process_id_for_testing(&info.id)
            .expect("read the process id")
            .expect("the platform reports a process id");

        assert_ne!(
            first_pid, second_pid,
            "a restart must run a new process rather than reuse the stopped one"
        );
        assert_eq!(restarted.exit_code, None);
        assert_eq!(restarted.status, ProcessState::Running);
        // The new process is usable.
        let token = format!("NEW-PROCESS-{}", std::process::id());
        fixture.expect_echo(&info.id, &token);
        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).ok();
    }

    #[test]
    fn stop_all_leaves_no_live_agent_process_behind() {
        // Spec section 15: the application exiting must not leave agents
        // running. The assertion is on the children this test started, so a
        // `claude.exe` the user happens to own is never inspected.
        let fixture = fixture("session-stop-all-children");
        let sessions: Vec<SessionInfo> = (0..3)
            .map(|index| {
                fixture
                    .start(
                        &format!("workspace-{index}"),
                        provider(
                            &format!("provider-{index}"),
                            &format!("https://provider-{index}.example.com"),
                            &format!("model-{index}"),
                            SECRET,
                        ),
                    )
                    .expect("start a session")
            })
            .collect();

        // Non-vacuous: all three agents are running before the shutdown.
        for info in &sessions {
            assert!(
                fixture
                    .manager
                    .process_is_alive_for_testing(&info.id)
                    .expect("poll an agent process"),
                "session {} must be running before stop_all",
                info.id
            );
        }

        let started = Instant::now();
        fixture.manager.stop_all(Duration::from_millis(1500));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "shutting down took {elapsed:?}, which is not bounded enough for application exit"
        );

        for info in &sessions {
            let stopped = fixture.manager.get(&info.id).unwrap();
            assert_eq!(stopped.status, ProcessState::Stopped, "session {}", info.id);
            assert!(stopped.exit_code.is_some());
            assert!(
                !fixture
                    .manager
                    .process_is_alive_for_testing(&info.id)
                    .expect("poll an agent process"),
                "stop_all left the agent process of session {} running",
                info.id
            );
        }

        // Application exit can deliver the event twice; nothing comes back.
        fixture.manager.stop_all(Duration::from_millis(100));
        for info in &sessions {
            assert!(!fixture
                .manager
                .process_is_alive_for_testing(&info.id)
                .expect("poll an agent process"));
        }
    }

    #[test]
    fn a_start_with_an_uninstalled_agent_fails_clearly_and_registers_no_session() {
        // Spec section 16: "Claude Code not installed". The real adapter reports
        // this itself (asserted in `agents::claude_code`); what is asserted here
        // is the session layer's half: the message survives unwrapped, the state
        // machine reaches `Starting -> Failed`, and no phantom session is left
        // for the UI to show.
        let fixture = fixture("session-not-installed");
        let error = fixture
            .start_with(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
                Box::new(FakeAgentAdapter::uninstalled()),
            )
            .expect_err("an uninstalled agent must not start a session");

        assert!(matches!(error, SessionError::Agent(AgentError::NotInstalled { .. })));
        let message = error.to_string();
        assert!(message.contains("Claude Code"), "{message}");
        assert!(message.contains("was not found on PATH"), "{message}");
        assert!(message.contains("install"), "the message must say what to do: {message}");
        assert!(!message.contains(SECRET), "leaked: {message}");
        assert!(!format!("{error:?}").contains(SECRET), "Debug leaked: {error:?}");

        assert!(fixture.manager.list().unwrap().is_empty());
        assert!(fixture.manager.is_empty());
    }

    #[test]
    fn a_start_with_an_unusable_executable_fails_clearly() {
        // Spec section 16: "Invalid executable". The PTY layer's message names
        // the program and explains itself; what must not happen is a start that
        // leaves a session behind or a credential in the text.
        let fixture = fixture("session-bad-executable");
        let error = fixture
            .start_with(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
                Box::new(FakeAgentAdapter::missing_program()),
            )
            .expect_err("a missing executable must not start a session");

        assert!(matches!(error, SessionError::Pty(PtyError::Spawn { .. })));
        let message = error.to_string();
        assert!(message.contains("could not start"), "{message}");
        assert!(
            message.contains("definitely-not-a-real-program-xyz"),
            "the message must name the program: {message}"
        );
        assert!(!message.contains(SECRET), "leaked: {message}");
        assert!(!format!("{error:?}").contains(SECRET), "Debug leaked: {error:?}");
        assert!(fixture.manager.list().unwrap().is_empty());
    }

    #[test]
    fn a_project_path_that_is_a_file_fails_the_start_with_the_folder_message() {
        // The "this project folder cannot be used" family at the session
        // boundary. A *deleted* folder is covered above; a path that is a file
        // and (in `agents::claude_code`) a folder that cannot be opened are the
        // other two, and all three must reach the user as themselves rather than
        // as a failed process creation (spec section 16).
        let fixture = fixture("session-project-is-a-file");
        let file = fixture.project.join("not-a-folder.txt");
        std::fs::write(&file, b"a file, not a project folder").unwrap();

        let error = fixture
            .manager
            .start(StartSessionRequest {
                workspace_id: "workspace-a".to_string(),
                project_path: file.clone(),
                provider: provider("provider-a", "https://a.example.com", "model-a", SECRET),
                adapter: Box::new(FakeAgentAdapter::interactive_shell()),
            })
            .expect_err("a file is not a project folder");

        assert!(matches!(
            error,
            SessionError::Agent(AgentError::ProjectDirectoryInvalid { .. })
        ));
        let message = error.to_string();
        assert!(message.contains("is not a folder"), "{message}");
        assert!(message.contains(&file.display().to_string()), "{message}");
        assert!(!message.contains(SECRET), "leaked: {message}");
        assert!(fixture.manager.list().unwrap().is_empty());
    }

    #[test]
    fn a_session_whose_configuration_directory_cannot_be_created_fails_clearly() {
        // Spec section 16: the per-session state is not optional (spec section 8
        // isolation depends on it), so a start that cannot create it must fail
        // with a message that names the path - not with a spawn error later.
        let root = TempDir::new("session-config-blocked");
        // A *file* where the shared sessions directory has to be.
        let blocked = root.join(SESSIONS_DIRECTORY_NAME);
        std::fs::write(&blocked, b"not a folder").unwrap();
        let project = TempDir::new("session-config-blocked-project");
        let manager = SessionManager::with_no_listener(&blocked);

        let error = manager
            .start(StartSessionRequest {
                workspace_id: "workspace-a".to_string(),
                project_path: project.path().to_path_buf(),
                provider: provider("provider-a", "https://a.example.com", "model-a", SECRET),
                adapter: Box::new(FakeAgentAdapter::interactive_shell()),
            })
            .expect_err("an unusable session directory must not start a session");

        assert!(matches!(
            error,
            SessionError::Agent(AgentError::ConfigDirectory { .. })
        ));
        let message = error.to_string();
        assert!(
            message.contains("could not create the agent configuration directory"),
            "{message}"
        );
        assert!(message.contains(SESSIONS_DIRECTORY_NAME), "{message}");
        assert!(!message.contains(SECRET), "leaked: {message}");
        // Nothing was registered, so the UI cannot show a phantom session.
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn no_session_debug_rendering_or_listing_carries_the_credential() {
        // Spec section 17, at the session layer: the runtime holds the
        // credential in its environment, so every rendering of it - and every
        // payload that crosses the Tauri boundary - must be free of the value.
        let fixture = fixture("session-debug-redaction");
        let info = fixture
            .start(
                "workspace-a",
                provider("provider-a", "https://a.example.com", "model-a", SECRET),
            )
            .expect("start a session");

        // The environment really does contain the credential (which is why the
        // renderings below have to be checked at all).
        let environment = fixture.manager.environment_for_testing(&info.id).unwrap();
        assert!(
            environment.iter().any(|(_name, value)| value == SECRET),
            "the session environment must carry the credential: {environment:?}"
        );

        let runtime_debug = fixture.manager.runtime_debug_for_testing(&info.id).unwrap();
        assert!(!runtime_debug.contains(SECRET), "the runtime Debug leaked: {runtime_debug}");
        assert!(
            runtime_debug.contains("input_environment"),
            "the runtime Debug should say that an environment exists: {runtime_debug}"
        );
        let manager_debug = format!("{:?}", fixture.manager);
        assert!(!manager_debug.contains(SECRET), "the manager Debug leaked: {manager_debug}");
        let info_debug = format!("{info:?}");
        assert!(!info_debug.contains(SECRET), "the session payload Debug leaked: {info_debug}");
        assert!(!serde_json::to_string(&info).unwrap().contains(SECRET));
        let listed = serde_json::to_string(&fixture.manager.list().unwrap()).unwrap();
        assert!(!listed.contains(SECRET), "the listed sessions leaked: {listed}");
        let states_debug = format!("{:?}", fixture.listener.states_for(&info.id));
        assert!(!states_debug.contains(SECRET), "the state events leaked: {states_debug}");

        fixture.manager.stop_with_grace(&info.id, SHORT_GRACE).ok();
    }

    #[test]
    fn stop_all_stops_every_session_and_is_safe_to_repeat() {
        let fixture = fixture("session-stop-all");
        let session_a = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        let session_b = fixture
            .start("workspace-b", provider("provider-b", "https://b.example.com", "model-b", "KEY_B"))
            .unwrap();

        let started = Instant::now();
        fixture.manager.stop_all(Duration::from_millis(500));
        let elapsed = started.elapsed();

        assert_eq!(
            fixture.manager.get(&session_a.id).unwrap().status,
            ProcessState::Stopped
        );
        assert_eq!(
            fixture.manager.get(&session_b.id).unwrap().status,
            ProcessState::Stopped
        );
        // Bounded on purpose: shutting down must not depend on how many agents
        // misbehave (spec section 15).
        assert!(
            elapsed < Duration::from_secs(10),
            "stop_all took {elapsed:?}, which is not bounded enough for application exit"
        );

        // Application exit can deliver the event twice; the second call must be
        // a no-op rather than an error or a hang.
        fixture.manager.stop_all(Duration::from_millis(50));
        assert_eq!(
            fixture.manager.get(&session_b.id).unwrap().status,
            ProcessState::Stopped
        );
    }

    #[test]
    fn listing_reports_sessions_in_creation_order() {
        let fixture = fixture("session-ordering");
        let first = fixture
            .start("workspace-a", provider("provider-a", "https://a.example.com", "model-a", "KEY_A"))
            .unwrap();
        let second = fixture
            .start("workspace-b", provider("provider-b", "https://b.example.com", "model-b", "KEY_B"))
            .unwrap();

        let listed = fixture.manager.list().unwrap();
        assert_eq!(
            listed.iter().map(|session| session.id.clone()).collect::<Vec<_>>(),
            vec![first.id.clone(), second.id.clone()]
        );

        fixture.manager.stop_all(Duration::from_millis(200));
    }

    #[test]
    fn session_payloads_are_camel_case_and_match_the_frontend_types() {
        // The wire contract `src/types/index.ts` declares: camelCase keys and
        // exactly these status spellings. `services/sessions.ts` drops anything
        // it does not recognise, so a rename here must fail loudly.
        let info = SessionInfo {
            id: "session-1".to_string(),
            workspace_id: "workspace-a".to_string(),
            status: ProcessState::Running,
            cols: 120,
            rows: 40,
            exit_code: None,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({
                "id": "session-1",
                "workspaceId": "workspace-a",
                "status": "running",
                "cols": 120,
                "rows": 40,
                "exitCode": null,
            })
        );

        let event = SessionStateEvent {
            session_id: "session-1".to_string(),
            status: ProcessState::Stopped,
            exit_code: Some(1),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "sessionId": "session-1",
                "status": "stopped",
                "exitCode": 1,
            })
        );
    }

    /// Every environment variable of this process, as a comparable snapshot.
    ///
    /// Taken with `vars_os` so a non-UTF-8 value cannot make the test panic.
    fn process_environment_snapshot() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut environment: Vec<_> = std::env::vars_os().collect();
        environment.sort();
        environment
    }
}
