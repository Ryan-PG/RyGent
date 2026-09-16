//! Agent process lifecycle (spec sections 15, 18, 19).
//!
//! This module owns three things:
//!
//! - [`ProcessState`]: the lifecycle state machine from spec section 15
//!   (`Created -> Starting -> Running -> Stopping -> Stopped`, plus
//!   `Starting -> Failed` and unexpected termination) modelled as an enum with
//!   an explicit, testable transition table.
//! - [`ProcessSpawn`]: the plain, fully-resolved description of the process to
//!   start (program, arguments, working directory, environment). It is
//!   deliberately a data-only struct so the PTY manager can spawn it and tests
//!   can substitute a fake command without an agent installed.
//! - [`ExitInfo`]: how a process ended, when the platform reports it.
//!
//! Platform differences in process creation and termination (spec section 18)
//! are not handled here: they live behind [`crate::pty`], which delegates to
//! `portable-pty` (Unix PTY vs Windows ConPTY).
//!
//! The third piece lives in [`guard`]: the OS-level backstop that keeps an agent
//! from being orphaned when this application is hard-killed, i.e. when the
//! graceful-then-forced stop never gets to run (spec section 15).

pub mod guard;

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Lifecycle state of one agent process (spec section 15).
///
/// Serialized `lowercase` because these exact spellings are the frontend's wire
/// contract (`TerminalSessionStatus` in `src/types/index.ts`); the enum is the
/// single source of truth for the state names.
///
/// ```text
/// Created -> Starting -> Running -> Stopping -> Stopped
///                |                       |
///                +-----> Failed <--------+   (and Running -> Failed/Stopped on
///                                             unexpected termination)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    /// A session record exists; nothing has been started.
    Created,
    /// A start attempt is in progress (spawn requested).
    Starting,
    /// The process is alive.
    Running,
    /// A stop was requested; the process is winding down.
    Stopping,
    /// The process is gone and this was expected (or an exit that succeeded).
    Stopped,
    /// The process could not be started, or died unexpectedly with a failure.
    Failed,
}

impl ProcessState {
    /// Every state, in lifecycle order (useful for tests and diagnostics).
    pub const ALL: [ProcessState; 6] = [
        ProcessState::Created,
        ProcessState::Starting,
        ProcessState::Running,
        ProcessState::Stopping,
        ProcessState::Stopped,
        ProcessState::Failed,
    ];

    /// Lowercase wire name (matches the serde representation and the frontend).
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessState::Created => "created",
            ProcessState::Starting => "starting",
            ProcessState::Running => "running",
            ProcessState::Stopping => "stopping",
            ProcessState::Stopped => "stopped",
            ProcessState::Failed => "failed",
        }
    }

    /// Whether the process is over (nothing will happen without a restart).
    pub fn is_terminal(self) -> bool {
        matches!(self, ProcessState::Stopped | ProcessState::Failed)
    }

    /// Whether a process is (or is about to be) alive.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            ProcessState::Starting | ProcessState::Running | ProcessState::Stopping
        )
    }

    /// Whether `self -> next` is a legal transition.
    ///
    /// The table is the spec's chain plus two documented extensions that
    /// `restart_session` needs (`Stopped -> Starting`, `Failed -> Starting`): a
    /// restart is a new start attempt *on the same session id*, so the id the
    /// frontend holds stays valid. Every other pair is rejected, which is what
    /// stops a stale supervisor thread or a racing stop from resurrecting a
    /// dead process.
    pub fn can_transition_to(self, next: ProcessState) -> bool {
        use ProcessState::*;
        match (self, next) {
            // The normal chain (spec section 15).
            (Created, Starting) => true,
            (Starting, Running) => true,
            // Start failure (spec section 15).
            (Starting, Failed) => true,
            // The child exited before the start was observed as running (a
            // very short-lived process, e.g. a CLI that printed a version and
            // quit). That is an unexpected termination of a successful start,
            // not a start failure.
            (Starting, Stopped) => true,
            // Stop requested, then the process is gone.
            (Running, Stopping) => true,
            (Stopping, Stopped) => true,
            // The process died on its own: treated as stopped when it exited
            // cleanly, failed when it did not (unexpected termination).
            (Running, Stopped) => true,
            (Running, Failed) => true,
            // A forced stop that could not reap the process is a failure.
            (Stopping, Failed) => true,
            // Restart (documented extension): a finished session starts again.
            (Stopped, Starting) => true,
            (Failed, Starting) => true,
            _ => false,
        }
    }

    /// Apply a transition, or report why it is not allowed.
    pub fn transition(self, next: ProcessState) -> Result<ProcessState, TransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(TransitionError { from: self, to: next })
        }
    }
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An attempt to move between two lifecycle states that the state machine
/// rejects. The message is user-facing and contains no process details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid process transition: {from} -> {to}")]
pub struct TransitionError {
    /// State the process was in.
    pub from: ProcessState,
    /// State that was requested.
    pub to: ProcessState,
}

/// How a process ended (spec section 15: "track exit status when available").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// Platform exit code, when one was reported. Clamped into `i32` because
    /// this is the field that crosses the Tauri boundary (`exitCode`), and the
    /// frontend type is a number.
    pub code: Option<i32>,
    /// The code exactly as the platform reported it.
    ///
    /// Windows reports `u32` status codes, and a terminator status (for example
    /// `0xC000013A`, "terminated by Ctrl-C") does not fit in an `i32` - it would
    /// reach the UI as a meaningless `2147483647`. Keeping the raw value lets
    /// [`ExitInfo::description`] say what actually happened.
    pub raw_code: Option<u32>,
    /// Signal name on Unix, when the process was signalled.
    pub signal: Option<String>,
    /// Whether this counts as a clean exit.
    pub success: bool,
}

impl ExitInfo {
    /// Build exit information from a platform exit code and optional signal.
    pub fn new(code: u32, signal: Option<String>) -> Self {
        let success = signal.is_none() && code == 0;
        Self {
            code: Some(i32::try_from(code).unwrap_or(i32::MAX)),
            raw_code: Some(code),
            signal,
            success,
        }
    }

    /// Short, secret-free description for logs and status lines.
    ///
    /// Every branch describes the *process*, never anything the session was
    /// given, so this is safe to log (spec section 17).
    pub fn description(&self) -> String {
        if let Some(signal) = &self.signal {
            return format!("terminated by {signal}");
        }
        match (self.code, self.raw_code) {
            (Some(0), _) => "exited normally".to_string(),
            // A code above `i32::MAX` is a platform termination status, not an
            // exit code a program chose: report it as a status so a crash is
            // diagnosable instead of showing a clamped number.
            (Some(_clamped), Some(raw)) if raw > i32::MAX as u32 => {
                format!("terminated by the operating system (status 0x{raw:08X})")
            }
            (Some(code), _) => format!("exited with code {code}"),
            (None, _) => "exited".to_string(),
        }
    }

    /// The state a session should end up in after an exit that was *not*
    /// requested by the user.
    ///
    /// A zero exit is "the agent finished" (`Stopped`); anything else is
    /// `Failed`, which is what the UI must show for a crash (spec section 15).
    pub fn state_for_unexpected_exit(&self) -> ProcessState {
        if self.success {
            ProcessState::Stopped
        } else {
            ProcessState::Failed
        }
    }
}

/// A fully-resolved process to start.
///
/// Plain data on purpose (spec section 20: prefer a working slice over an
/// abstraction): the agent adapter produces one, the PTY manager consumes one,
/// and a test can build one pointing at any executable.
///
/// # No secrets in logs
///
/// [`ProcessSpawn::environment`] carries the session's credential variables, so
/// `Debug` is hand-written and prints variable *names* only (spec sections 5,
/// 16, 17).
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessSpawn {
    /// Executable to run (an absolute path, or a name resolved through PATH).
    pub program: PathBuf,
    /// Base arguments. Empty for an interactive agent CLI (spec section 6).
    pub args: Vec<String>,
    /// Working directory: the workspace's project folder.
    pub cwd: PathBuf,
    /// Environment layered on top of the inherited parent environment. Later
    /// entries win, which is how a provider's extra variables override the
    /// adapter's defaults.
    pub environment: Vec<(String, String)>,
}

impl ProcessSpawn {
    /// Assemble a spawn description.
    pub fn new(
        program: impl Into<PathBuf>,
        args: Vec<String>,
        cwd: impl Into<PathBuf>,
        environment: Vec<(String, String)>,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: cwd.into(),
            environment,
        }
    }

    /// The last value set for `name`, i.e. what the child process would see.
    pub fn environment_value(&self, name: &str) -> Option<&str> {
        self.environment
            .iter()
            .filter(|(key, _value)| key == name)
            .next_back()
            .map(|(_key, value)| value.as_str())
    }
}

impl fmt::Debug for ProcessSpawn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .environment
            .iter()
            .map(|(name, _value)| name.as_str())
            .collect();
        formatter
            .debug_struct("ProcessSpawn")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field(
                "environment",
                &format_args!("<{} variables: {}>", names.len(), names.join(", ")),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_chain_is_allowed() {
        use ProcessState::*;
        let chain = [
            (Created, Starting),
            (Starting, Running),
            (Running, Stopping),
            (Stopping, Stopped),
        ];
        for (from, to) in chain {
            assert!(
                from.can_transition_to(to),
                "{from} -> {to} must be allowed"
            );
            assert_eq!(from.transition(to), Ok(to));
        }
    }

    #[test]
    fn failure_and_unexpected_termination_paths_are_allowed() {
        use ProcessState::*;
        // Spec section 15: Starting -> Failed.
        assert!(Starting.can_transition_to(Failed));
        // Unexpected termination while running: clean exit vs crash.
        assert!(Running.can_transition_to(Stopped));
        assert!(Running.can_transition_to(Failed));
        // A process that exits before the start was observed as running.
        assert!(Starting.can_transition_to(Stopped));
        // A forced stop that still could not be reaped.
        assert!(Stopping.can_transition_to(Failed));
    }

    #[test]
    fn restart_is_the_only_way_back_from_a_terminal_state() {
        use ProcessState::*;
        assert!(Stopped.can_transition_to(Starting));
        assert!(Failed.can_transition_to(Starting));
        // ... and nothing else: a stopped process must be restarted, never
        // resumed or "re-run" in place.
        for next in [Running, Stopping, Stopped, Failed, Created] {
            assert!(
                !Stopped.can_transition_to(next),
                "Stopped -> {next} must be rejected"
            );
            assert!(!Failed.can_transition_to(next), "Failed -> {next} must be rejected");
        }
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        use ProcessState::*;
        let invalid = [
            (Created, Running),
            (Created, Stopping),
            (Created, Stopped),
            (Created, Failed),
            (Starting, Stopping),
            (Running, Created),
            (Running, Starting),
            (Stopping, Running),
            (Stopping, Starting),
        ];
        for (from, to) in invalid {
            assert!(
                !from.can_transition_to(to),
                "{from} -> {to} must be rejected"
            );
            let error = from.transition(to).expect_err("must be rejected");
            assert_eq!(error.from, from);
            assert_eq!(error.to, to);
            assert_eq!(
                error.to_string(),
                format!("invalid process transition: {from} -> {to}")
            );
        }
    }

    #[test]
    fn a_state_never_transitions_to_itself() {
        for state in ProcessState::ALL {
            assert!(!state.can_transition_to(state), "{state} -> {state}");
        }
    }

    #[test]
    fn terminal_and_live_classification_matches_the_spec() {
        assert!(ProcessState::Created.is_terminal() == false);
        assert!(ProcessState::Created.is_live() == false);
        assert!(ProcessState::Starting.is_live());
        assert!(ProcessState::Running.is_live());
        assert!(ProcessState::Stopping.is_live());
        assert!(ProcessState::Stopped.is_terminal());
        assert!(ProcessState::Failed.is_terminal());
        // No state is both.
        for state in ProcessState::ALL {
            assert!(!(state.is_terminal() && state.is_live()), "{state}");
        }
    }

    #[test]
    fn states_serialize_to_the_frontend_wire_names() {
        // `src/types/index.ts` accepts exactly these strings.
        let expected = [
            (ProcessState::Created, "\"created\""),
            (ProcessState::Starting, "\"starting\""),
            (ProcessState::Running, "\"running\""),
            (ProcessState::Stopping, "\"stopping\""),
            (ProcessState::Stopped, "\"stopped\""),
            (ProcessState::Failed, "\"failed\""),
        ];
        for (state, json) in expected {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
            assert_eq!(state.as_str(), json.trim_matches('"'));
        }
    }

    #[test]
    fn exit_info_classifies_clean_and_failed_exits() {
        use ProcessState::*;

        let clean = ExitInfo::new(0, None);
        assert!(clean.success);
        assert_eq!(clean.code, Some(0));
        assert_eq!(clean.description(), "exited normally");
        assert_eq!(clean.state_for_unexpected_exit(), Stopped);

        let crash = ExitInfo::new(1, None);
        assert!(!crash.success);
        assert_eq!(crash.description(), "exited with code 1");
        assert_eq!(crash.state_for_unexpected_exit(), Failed);

        let signalled = ExitInfo::new(1, Some("SIGHUP".to_string()));
        assert!(!signalled.success);
        assert_eq!(signalled.description(), "terminated by SIGHUP");
        assert_eq!(signalled.state_for_unexpected_exit(), Failed);
    }

    #[test]
    fn a_windows_termination_status_is_described_instead_of_clamped() {
        // Spec section 16: "Process crash". Windows reports `u32` statuses, and
        // the ones that matter here are above `i32::MAX` - so the number the UI
        // receives is necessarily clamped and would otherwise be meaningless.
        // The description keeps the real status, which is what makes a crash
        // diagnosable from a log line.
        //
        // 0xC000013A is STATUS_CONTROL_C_EXIT, the status a console process is
        // given when its console goes away; portable-pty hands it over as a u32.
        let terminated_by_os = ExitInfo::new(0xC000_013A, None);
        assert!(!terminated_by_os.success);
        assert_eq!(terminated_by_os.code, Some(i32::MAX), "the wire field stays a number");
        assert_eq!(terminated_by_os.raw_code, Some(0xC000_013A));
        assert_eq!(
            terminated_by_os.description(),
            "terminated by the operating system (status 0xC000013A)"
        );
        // A crash is still a failure for the lifecycle, not a clean stop.
        assert_eq!(
            terminated_by_os.state_for_unexpected_exit(),
            ProcessState::Failed
        );

        // A status that fits in an `i32` keeps its plain description, and the
        // raw field mirrors the wire field.
        let plain = ExitInfo::new(7, None);
        assert_eq!(plain.raw_code, Some(7));
        assert_eq!(plain.description(), "exited with code 7");

        // A signal still wins: the description names what killed the process.
        let signalled = ExitInfo::new(0, Some("SIGKILL".to_string()));
        assert_eq!(signalled.description(), "terminated by SIGKILL");
        assert!(!signalled.success, "a signalled process did not exit cleanly");
    }

    #[test]
    fn process_spawn_redacts_environment_values() {
        // A spawn description carries the session credential; it must never be
        // rendered into a log line (spec sections 5, 16, 17).
        let spawn = ProcessSpawn::new(
            "claude",
            Vec::new(),
            "/work/project-alpha",
            vec![
                ("ANTHROPIC_BASE_URL".to_string(), "https://a.example.com".to_string()),
                ("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test-DO-NOT-LOG".to_string()),
            ],
        );

        let rendered = format!("{spawn:?}");
        assert!(!rendered.contains("sk-test-DO-NOT-LOG"), "leaked: {rendered}");
        assert!(!rendered.contains("https://a.example.com"), "leaked: {rendered}");
        assert!(rendered.contains("ANTHROPIC_AUTH_TOKEN"));
        assert!(rendered.contains("/work/project-alpha"));

        // The value is still available to the spawner, and later entries win.
        assert_eq!(spawn.environment_value("ANTHROPIC_AUTH_TOKEN"), Some("sk-test-DO-NOT-LOG"));
        let overridden = ProcessSpawn::new(
            "claude",
            Vec::new(),
            "/work/project-alpha",
            vec![
                ("ANTHROPIC_MODEL".to_string(), "model-a".to_string()),
                ("ANTHROPIC_MODEL".to_string(), "model-b".to_string()),
            ],
        );
        assert_eq!(overridden.environment_value("ANTHROPIC_MODEL"), Some("model-b"));
    }
}
