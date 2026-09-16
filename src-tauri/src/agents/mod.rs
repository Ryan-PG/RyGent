//! Agent adapter abstraction (spec sections 6, 7, 19, 25).
//!
//! [`AgentAdapter`] is the seam that lets the workspace host different AI
//! coding agents. For the MVP only [`claude_code::ClaudeCodeAdapter`] is
//! implemented; future adapters (Codex, Gemini, ...) plug in behind the same
//! trait without refactorings elsewhere.
//!
//! An adapter answers three questions, all of which the session manager needs
//! before it can start anything (spec section 6):
//!
//! 1. Is the agent installed, and where is its executable? ([`AgentAdapter::is_installed`],
//!    [`AgentAdapter::executable_path`])
//! 2. What environment does one session need? ([`AgentAdapter::build_environment`])
//! 3. What exactly should be spawned? ([`AgentAdapter::spawn_description`])
//!
//! Environment construction lives here rather than in the frontend because
//! environment isolation is a core requirement (spec sections 7, 8): the
//! adapter receives a [`ResolvedProvider`] (profile + API key in memory) and
//! returns the exact `(name, value)` pairs for one session's process. Values
//! may contain secrets, so callers must never log them or send them to the UI.

pub mod claude_code;

use std::fmt;
use std::path::PathBuf;

use crate::process::ProcessSpawn;
use crate::providers::ResolvedProvider;

/// Lifecycle state of an agent, re-exported from the process module.
///
/// One enum for the whole core: spec section 15's state machine is owned by
/// [`crate::process`] and the authoritative per-session state lives in
/// [`crate::sessions`]. This alias keeps [`AgentAdapter::state`] meaningful
/// without inventing a second, divergent lifecycle vocabulary.
pub use crate::process::ProcessState as AgentState;

/// Everything a session must supply before an agent can be spawned.
///
/// A plain struct (no DB, no Tauri, no clock) so a test can build one and so an
/// adapter never reaches outside its inputs.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The workspace's project folder. Becomes the agent's working directory.
    pub project_path: PathBuf,
    /// Per-session configuration directory (spec section 8: session isolation).
    /// Claude Code uses it for `CLAUDE_CONFIG_DIR`.
    pub config_dir: PathBuf,
    /// The provider profile plus the API key read from the secret store.
    pub provider: ResolvedProvider,
}

/// Errors produced by an agent adapter (spec section 16).
///
/// Messages are user-facing and secret-free: they describe the executable, the
/// project folder, or the configuration directory - never an environment value
/// (spec section 17).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// The agent CLI could not be found on PATH (spec section 16: "Claude Code
    /// not installed").
    #[error("{agent} was not found on PATH - install it and restart the application")]
    NotInstalled {
        /// Human-readable agent name, e.g. "Claude Code".
        agent: &'static str,
    },

    /// The workspace's project folder does not exist any more (spec section 16:
    /// "Project directory deleted").
    #[error("the project folder does not exist: {path}")]
    ProjectDirectoryMissing {
        /// The missing folder.
        path: String,
    },

    /// The project folder exists but is not a directory.
    #[error("the project path is not a folder: {path}")]
    ProjectDirectoryInvalid {
        /// The offending path.
        path: String,
    },

    /// The project folder is a directory but cannot be opened, so the agent
    /// could not use it as its working directory (spec section 16: "Permission
    /// errors"). Carries the platform text for the operator while the first
    /// half of the message stays actionable for the user.
    #[error(
        "the project folder cannot be opened: {path} - check that you have permission to read it ({message})"
    )]
    ProjectDirectoryNotAccessible {
        /// The folder that could not be opened.
        path: String,
        /// Platform error text (a permission problem, a device error, ...).
        message: String,
    },

    /// The per-session configuration directory could not be created.
    #[error("could not create the agent configuration directory {path}: {message}")]
    ConfigDirectory {
        /// Directory that could not be created.
        path: String,
        /// Platform error text.
        message: String,
    },
}

/// Abstraction over a supported AI coding agent CLI.
///
/// `Send + Sync` because a session owns its adapter and sessions are shared
/// across the command threads.
pub trait AgentAdapter: Send + Sync {
    /// Stable adapter id persisted in `workspaces.agent_id`.
    fn id(&self) -> &'static str;

    /// Human-readable agent name, e.g. "Claude Code".
    fn name(&self) -> &'static str;

    /// Whether the agent CLI is installed and discoverable on this machine.
    fn is_installed(&self) -> bool;

    /// Path to the discovered agent executable, if any.
    fn executable_path(&self) -> Option<PathBuf>;

    /// Build the per-session environment variables for spawning this agent
    /// against the given provider profile.
    ///
    /// Returns `(name, value)` pairs layered on top of an otherwise inherited
    /// (or deliberately filtered) parent environment. The returned values may
    /// contain secrets and must never be logged or sent to the frontend
    /// (spec sections 7, 17).
    fn build_environment(&self, provider: &ResolvedProvider) -> Vec<(String, String)>;

    /// Fully resolve the process to start for one session: executable, base
    /// arguments, working directory (the project folder) and environment.
    ///
    /// Fails with [`AgentError`] when the agent is missing or the session's
    /// paths are unusable, which is what turns into `Starting -> Failed`
    /// (spec section 15).
    fn spawn_description(&self, request: &SpawnRequest) -> Result<ProcessSpawn, AgentError>;

    /// Current lifecycle state of this adapter's agent process.
    ///
    /// Informational: the authoritative state machine for a running session is
    /// owned by [`crate::sessions`], which supervises the real child process.
    fn state(&self) -> AgentState;

    /// Base arguments for an interactive session. Empty for the MVP: Claude
    /// Code is started as an interactive CLI with no extra flags (spec section
    /// 6), and per-session configuration travels in the environment.
    fn base_arguments(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Test doubles shared by the `pty` and `sessions` suites.
///
/// `FakeAgentAdapter` is the "substitute a fake command" seam: it builds its
/// spawn description with the *real* Claude Code builder
/// ([`claude_code::ClaudeCodeAdapter::build_spawn`]) but points it at a
/// harmless, always-present program, so PTY and session behaviour can be tested
/// on a machine with no Claude Code installation and no network (spec section
/// 23).
#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::agents::claude_code::ClaudeCodeAdapter;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// An interactive shell that stays alive until it is stopped.
    ///
    /// Used as the stand-in for an interactive agent CLI: both shells read from
    /// the PTY, so input written to the session can be observed in the output.
    pub fn interactive_shell_command() -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            // `/k` keeps the command interpreter running after the command.
            (
                PathBuf::from("cmd.exe"),
                vec!["/k".to_string()],
            )
        } else {
            // A POSIX shell with no arguments reads commands from stdin.
            (PathBuf::from("/bin/sh"), Vec::new())
        }
    }

    /// A short-lived command that prints `token` and exits.
    pub fn echo_command(token: &str) -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            (
                PathBuf::from("cmd.exe"),
                vec!["/c".to_string(), format!("echo {token}")],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec!["-c".to_string(), format!("echo {token}")],
            )
        }
    }

    /// A short-lived command that exits with a specific status code, without
    /// printing anything (spec section 15: unexpected termination).
    pub fn exit_command(code: i32) -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            (
                PathBuf::from("cmd.exe"),
                vec!["/c".to_string(), format!("exit {code}")],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec!["-c".to_string(), format!("exit {code}")],
            )
        }
    }

    /// A program name that deliberately does not resolve anywhere, so the spawn
    /// itself fails (spec section 16: "Invalid executable").
    pub fn missing_program_command() -> (PathBuf, Vec<String>) {
        let program = if cfg!(windows) {
            "definitely-not-a-real-program-xyz.exe"
        } else {
            "definitely-not-a-real-program-xyz"
        };
        (PathBuf::from(program), Vec::new())
    }

    /// An adapter whose executable is a real, trivially available command but
    /// whose environment construction is the real Claude Code one.
    ///
    /// `failure` is the seam for the agent errors a start must survive: a fake
    /// that reports "the CLI is not installed" or "the folder is gone" lets the
    /// session tests prove `Starting -> Failed` and the message the user sees
    /// without a broken machine (spec sections 15, 16, 23).
    ///
    /// `crash_first` is the seam for a *process* that dies on its own: the first
    /// spawn description is that command and every later one is `program`/`args`.
    /// Because the runtime keeps its adapter across a restart, one session id
    /// can be observed crashing (`Failed`) and then running a working process -
    /// which is what "restart after a crash" needs (spec section 15).
    pub struct FakeAgentAdapter {
        program: PathBuf,
        args: Vec<String>,
        failure: Option<AgentError>,
        crash_first: Option<(PathBuf, Vec<String>)>,
        /// How many spawn descriptions this adapter has produced.
        spawns: Arc<AtomicU32>,
    }

    impl FakeAgentAdapter {
        /// A description that spawns the interactive shell above.
        fn shell() -> Self {
            let (program, args) = interactive_shell_command();
            Self {
                program,
                args,
                failure: None,
                crash_first: None,
                spawns: Arc::new(AtomicU32::new(0)),
            }
        }

        /// An adapter that spawns the interactive shell above.
        pub fn interactive_shell() -> Self {
            Self::shell()
        }

        /// An adapter that spawns a short-lived echo command.
        pub fn echo(token: &str) -> Self {
            let (program, args) = echo_command(token);
            Self {
                program,
                args,
                failure: None,
                crash_first: None,
                spawns: Arc::new(AtomicU32::new(0)),
            }
        }

        /// An adapter whose process exits on its own with `code` - a crash when
        /// the code is non-zero.
        pub fn exit_with(code: i32) -> Self {
            let (program, args) = exit_command(code);
            Self {
                program,
                args,
                failure: None,
                crash_first: None,
                spawns: Arc::new(AtomicU32::new(0)),
            }
        }

        /// An adapter whose **first** process exits with `code` and whose later
        /// ones are the interactive shell: the shape a session has when it is
        /// restarted after a crash.
        pub fn crash_then_run_shell(code: i32) -> Self {
            let mut adapter = Self::shell();
            adapter.crash_first = Some(exit_command(code));
            adapter
        }

        /// An adapter pointing at a program that does not exist, so the PTY
        /// spawn fails the way a broken installation would.
        pub fn missing_program() -> Self {
            let (program, args) = missing_program_command();
            Self {
                program,
                args,
                failure: None,
                crash_first: None,
                spawns: Arc::new(AtomicU32::new(0)),
            }
        }

        /// An adapter that reports the agent as not installed (spec section 16).
        pub fn uninstalled() -> Self {
            let mut adapter = Self::shell();
            adapter.failure = Some(AgentError::NotInstalled { agent: "Claude Code" });
            adapter
        }
    }

    impl AgentAdapter for FakeAgentAdapter {
        fn id(&self) -> &'static str {
            "fake-agent"
        }

        fn name(&self) -> &'static str {
            "Fake Agent"
        }

        fn is_installed(&self) -> bool {
            self.failure.is_none()
        }

        fn executable_path(&self) -> Option<PathBuf> {
            Some(self.program.clone())
        }

        fn build_environment(&self, provider: &ResolvedProvider) -> Vec<(String, String)> {
            ClaudeCodeAdapter::new().build_environment(provider)
        }

        fn spawn_description(&self, request: &SpawnRequest) -> Result<ProcessSpawn, AgentError> {
            if let Some(failure) = self.failure.clone() {
                return Err(failure);
            }
            let count = self.spawns.fetch_add(1, Ordering::SeqCst);
            let (program, args) = match (&self.crash_first, count) {
                (Some((program, args)), 0) => (program.clone(), args.clone()),
                _ => (self.program.clone(), self.args.clone()),
            };
            // The real builder: working directory, environment (including
            // CLAUDE_CONFIG_DIR) and validation are the production code paths.
            ClaudeCodeAdapter::build_spawn(&program, &args, request)
        }

        fn state(&self) -> AgentState {
            AgentState::Created
        }

        fn base_arguments(&self) -> Vec<String> {
            self.args.clone()
        }
    }
}

impl fmt::Debug for dyn AgentAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAdapter")
            .field("id", &self.id())
            .field("name", &self.name())
            .field("installed", &self.is_installed())
            .finish()
    }
}
