//! Agent adapter abstraction (spec sections 6, 7, 19, 25).
//!
//! [`AgentAdapter`] is the seam that lets the workspace host different AI
//! coding agents. Two are implemented - [`claude_code::ClaudeCodeAdapter`] and
//! [`codex::CodexAdapter`] - and further adapters (Gemini, ...) plug in behind
//! the same trait, plus one entry in [`AGENT_IDS`], without refactorings
//! elsewhere: nothing in [`crate::sessions`], [`crate::pty`] or the command
//! layer names a specific agent.
//!
//! An adapter answers three questions, all of which the session manager needs
//! before it can start anything (spec section 6):
//!
//! 1. Is the agent installed, and where is its executable? ([`AgentAdapter::is_installed`],
//!    [`AgentAdapter::executable_path`])
//! 2. What environment does one session need? ([`AgentAdapter::build_environment`])
//! 3. What exactly should be spawned? ([`AgentAdapter::spawn_description`])
//!
//! Question 3 is answered by three shared pieces rather than by each adapter
//! from scratch: [`executable`] finds any agent CLI on `PATH`, [`spawn`] turns a
//! project folder, a configuration directory and an environment into a
//! [`ProcessSpawn`], and the adapter supplies only what is genuinely
//! agent-specific - the environment variables and the arguments.
//!
//! Environment construction lives here rather than in the frontend because
//! environment isolation is a core requirement (spec sections 7, 8): the
//! adapter receives a [`ResolvedProvider`] (profile + API key in memory) and
//! returns the exact `(name, value)` pairs for one session's process. Values
//! may contain secrets, so callers must never log them or send them to the UI.

pub mod claude_code;
pub mod codex;
pub(crate) mod executable;
pub(crate) mod spawn;

use std::fmt;
use std::path::PathBuf;

use crate::process::ProcessSpawn;
use crate::providers::ResolvedProvider;

use claude_code::ClaudeCodeAdapter;
use codex::CodexAdapter;

/// Every agent id this build implements, in the order the UI offers them.
///
/// This is the single place a new agent is registered: [`is_supported`],
/// [`adapter_for`] and [`descriptors`] all derive from it, so "the agents the
/// frontend may offer" and "the agents a session can start" cannot drift apart.
pub const AGENT_IDS: [&str; 2] = [claude_code::AGENT_ID, codex::AGENT_ID];

/// Whether `agent_id` names an agent this build implements.
///
/// Used to validate what a user (or an older database row) asks for before it is
/// persisted, so an unknown id is refused at the boundary with a message that
/// names it rather than failing later at spawn time (spec sections 9, 24).
pub fn is_supported(agent_id: &str) -> bool {
    adapter(agent_id).is_some()
}

/// The adapter for a persisted `agent_id`.
///
/// The error text names the unknown id, so a workspace written by a newer build
/// - or a hand-edited database row - says exactly what it could not resolve.
pub fn adapter_for(agent_id: &str) -> Result<Box<dyn AgentAdapter>, String> {
    adapter(agent_id).ok_or_else(|| format!("unsupported agent: {}", agent_id.trim()))
}

/// Build the adapter for an id, or `None` when the id is not supported.
fn adapter(agent_id: &str) -> Option<Box<dyn AgentAdapter>> {
    match agent_id.trim() {
        claude_code::AGENT_ID => Some(Box::new(ClaudeCodeAdapter::new())),
        codex::AGENT_ID => Some(Box::new(CodexAdapter::new())),
        _other => None,
    }
}

/// One supported agent, as the frontend needs to describe it.
///
/// Deliberately holds no provider or credential information: the agent list is
/// about the *machine* (is this CLI installed, where is it), never about a
/// session's secret (spec section 17).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDescriptor {
    /// Stable adapter id, e.g. `claude-code`; what a workspace persists.
    pub id: &'static str,
    /// Human-readable name, e.g. `Claude Code`.
    pub name: &'static str,
    /// Whether the CLI was found on this machine.
    pub installed: bool,
    /// Resolved executable path; `null` when the CLI is not installed.
    pub executable_path: Option<String>,
}

/// Every supported agent with its installation state on this machine.
///
/// The frontend uses this to offer a real agent choice and to explain, before a
/// session is created, that a CLI it would need is not installed (spec section
/// 16: "the failure must be understandable before it happens").
pub fn descriptors() -> Vec<AgentDescriptor> {
    AGENT_IDS
        .iter()
        .filter_map(|agent_id| adapter(agent_id))
        .map(|adapter| AgentDescriptor {
            id: adapter.id(),
            name: adapter.name(),
            installed: adapter.is_installed(),
            executable_path: adapter
                .executable_path()
                .map(|path| path.display().to_string()),
        })
        .collect()
}

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
    /// Each agent has its own name for it - Claude Code reads
    /// `CLAUDE_CONFIG_DIR`, Codex reads `CODEX_HOME` - and the adapter is what
    /// injects it (see [`spawn::build_session_spawn`]).
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
    /// The agent CLI could not be found on PATH (spec section 16: "the agent is
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

    /// Base arguments for an interactive session, shared by every session of
    /// this agent.
    ///
    /// Empty for both implemented agents: they are started as interactive CLIs
    /// with no extra flags (spec section 6). Anything that varies *per session*
    /// (Codex's model flag, for example) belongs in
    /// [`AgentAdapter::spawn_description`], which receives the session's
    /// provider.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_offers_claude_code_and_codex() {
        // The agents the UI may offer and the agents a session may start come
        // from this one list (spec sections 6, 24).
        assert_eq!(AGENT_IDS, ["claude-code", "codex"]);
        assert!(is_supported("claude-code"));
        assert!(is_supported("codex"));
        assert!(!is_supported("gemini"));
        assert!(!is_supported(""));
    }

    #[test]
    fn every_registered_id_resolves_to_an_adapter_with_that_id() {
        // Guards the registry against drift: an id in `AGENT_IDS` with no
        // adapter would silently vanish from the UI, and an adapter registered
        // under the wrong id would be stored in `workspaces.agent_id` under a
        // name nothing can resolve later.
        for agent_id in AGENT_IDS {
            let adapter = adapter_for(agent_id)
                .unwrap_or_else(|error| panic!("{agent_id} is registered but not startable: {error}"));
            assert_eq!(adapter.id(), agent_id);
            assert!(
                !adapter.name().trim().is_empty(),
                "{agent_id} needs a display name for the session UI"
            );
        }
    }

    #[test]
    fn an_unknown_agent_is_refused_with_its_id() {
        let error = adapter_for("gemini").expect_err("an unimplemented agent must be refused");
        assert_eq!(error, "unsupported agent: gemini");
        assert!(!format!("{error:?}").contains("KEY"), "leaked: {error:?}");
    }

    /// Ids arrive from the database and from the frontend, both of which may
    /// carry incidental whitespace; a trailing space must not turn a valid agent
    /// into an unsupported one.
    #[test]
    fn lookup_ignores_surrounding_whitespace() {
        assert!(is_supported("  codex  "));
        assert_eq!(adapter_for(" codex ").unwrap().id(), "codex");
        // A blank id is still refused - it names no agent at all.
        assert!(adapter_for("   ").is_err());
    }

    #[test]
    fn descriptors_describe_every_supported_agent() {
        let descriptors = descriptors();
        assert_eq!(descriptors.len(), AGENT_IDS.len());

        let ids: Vec<&str> = descriptors.iter().map(|agent| agent.id).collect();
        assert_eq!(ids, AGENT_IDS);

        for descriptor in &descriptors {
            assert!(!descriptor.name.trim().is_empty());
            // Installation state and the path must agree: a "found" agent with
            // no path (or the reverse) would show a contradictory hint in the
            // UI. Machine-dependent, so both outcomes are accepted.
            assert_eq!(
                descriptor.installed,
                descriptor.executable_path.is_some(),
                "{} reports an inconsistent installation state",
                descriptor.id
            );
            if let Some(path) = &descriptor.executable_path {
                println!("{}: installed at {path}", descriptor.id);
            } else {
                println!("{}: not installed on this machine", descriptor.id);
            }
        }
    }

    /// The wire contract `src/types/index.ts` mirrors: camelCase field names and
    /// no nested secret-bearing value (spec section 17).
    #[test]
    fn descriptors_are_serialized_camel_case_for_the_frontend() {
        let json = serde_json::to_value(AgentDescriptor {
            id: "codex",
            name: "Codex",
            installed: false,
            executable_path: None,
        })
        .expect("serialize a descriptor");

        assert_eq!(
            json,
            serde_json::json!({
                "id": "codex",
                "name": "Codex",
                "installed": false,
                "executablePath": null,
            })
        );
    }
}
