//! PTY handling for interactive agent CLIs (spec sections 11, 18, 19).
//!
//! One [`PtyProcess`] is one pseudo-terminal plus the process spawned inside
//! it. The module is deliberately small and platform-neutral: Unix `openpty`
//! and Windows ConPTY differences are owned by `portable-pty` behind
//! [`portable_pty::native_pty_system`], and **no terminal emulation happens
//! here** (spec section 11: use a mature PTY approach, never reimplement a
//! terminal). Bytes go in ([`PtyProcess::write`]), bytes come out
//! (the `on_output` callback), and the size is forwarded to the kernel
//! ([`PtyProcess::resize`]) so full-screen TUIs reflow.
//!
//! Responsibilities:
//!
//! - allocate a PTY pair at a given size,
//! - spawn a [`ProcessSpawn`] into the slave side,
//! - put the child under OS-level orphan protection (see [`ProcessGuard`]),
//! - pump output on a dedicated reader thread in bounded chunks,
//! - expose `write` / `resize` / `kill` and bounded exit waiting,
//! - make it impossible to orphan the child (a killed child on `Drop`, plus the
//!   OS-level guard for the case where this application never gets to run `Drop`).
//!
//! Environment handling is *not* here: the caller passes the exact environment
//! a session needs, which is how per-session isolation is enforced (spec
//! sections 7, 8). `portable-pty` seeds the child environment from the
//! application's own environment and then applies these overrides, so the
//! application's process environment is never modified.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::process::guard::ProcessGuard;
use crate::process::{ExitInfo, ProcessSpawn};

/// Columns a fresh PTY gets before the frontend reports the real geometry.
pub const DEFAULT_COLUMNS: u16 = 80;

/// Rows a fresh PTY gets before the frontend reports the real geometry.
pub const DEFAULT_ROWS: u16 = 24;

/// Upper bound for a reported dimension. Anything larger is clamped rather than
/// rejected: a bogus size must never fail a session (spec section 16).
pub const MAX_DIMENSION: u16 = 1000;

/// Bytes read from the PTY per wake-up. Large enough to keep up with a TUI
/// redraw, small enough that the frontend receives responsive chunks.
const READ_BUFFER_SIZE: usize = 8192;

/// How often a bounded exit wait polls the child.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long [`Drop`] waits for the reader thread before detaching it.
///
/// Dropping a PTY must never block: on Windows the reader can stay parked in
/// the OS until the pseudo-console is closed, which is already requested by the
/// time this timeout is used. A detached reader thread exits on its own once
/// the pipe is torn down and cannot keep the process alive.
const READER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);

/// Ctrl-C (`ETX`), the interrupt an interactive CLI understands.
const INTERRUPT: u8 = 0x03;

/// Errors produced while owning a PTY (spec section 16).
///
/// Every message is safe to show to a user and to log: they come from
/// `portable-pty`/`std::io` and describe the *command*, never the environment,
/// so a session credential cannot leak through them (spec section 17).
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// The pseudo-terminal itself could not be allocated.
    #[error("could not allocate a pseudo-terminal: {0}")]
    Open(String),

    /// The child process could not be created.
    #[error("could not start `{program}`: {message}")]
    Spawn {
        /// Executable that failed to start.
        program: String,
        /// Platform error text.
        message: String,
    },

    /// Input could not be written to the PTY.
    #[error("could not write to the agent process: {0}")]
    Write(String),

    /// The terminal could not be resized.
    #[error("could not resize the agent terminal: {0}")]
    Resize(String),

    /// The output reader thread could not be started.
    #[error("could not start the PTY output reader: {0}")]
    Reader(String),

    /// The process could not be signalled to terminate.
    #[error("could not terminate the agent process: {0}")]
    Kill(String),

    /// Waiting for the process failed.
    #[error("could not read the agent process exit status: {0}")]
    Wait(String),
}

/// Size of the visible terminal area, in character cells.
///
/// A small owned type rather than `portable_pty::PtySize` so the rest of the
/// core (and the wire format) is free of PTY-library types, and so an
/// out-of-range value from the frontend is clamped instead of erroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    /// Build a size, clamping each dimension into `1..=MAX_DIMENSION`.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.clamp(1, MAX_DIMENSION),
            rows: rows.clamp(1, MAX_DIMENSION),
        }
    }

    /// Build a size from the numbers a frontend sends (`u32`, possibly 0).
    pub fn from_columns_rows(cols: u32, rows: u32) -> Self {
        let clamp = |value: u32| value.clamp(1, MAX_DIMENSION as u32) as u16;
        Self {
            cols: clamp(cols),
            rows: clamp(rows),
        }
    }

    /// Columns.
    pub fn cols(self) -> u16 {
        self.cols
    }

    /// Rows.
    pub fn rows(self) -> u16 {
        self.rows
    }

    /// The `portable-pty` equivalent; pixel dimensions are unused.
    fn to_pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLUMNS,
            rows: DEFAULT_ROWS,
        }
    }
}

/// A running process inside its own pseudo-terminal.
///
/// The struct owns both ends it needs: the master side (to write, resize and
/// keep the pseudo-console alive) and the child handle (to poll and kill). The
/// reader thread owns a cloned read handle, so output keeps flowing while the
/// caller holds the session lock.
pub struct PtyProcess {
    /// Master side. `Option` only so [`Drop`] can close it *before* joining the
    /// reader: closing the pseudo-console is what releases a reader parked in
    /// the OS on Windows.
    master: Option<Box<dyn MasterPty + Send>>,
    /// Write handle. Dropping it closes the agent's input (EOF).
    writer: Option<Box<dyn Write + Send>>,
    /// Child process handle.
    child: Box<dyn Child + Send + Sync>,
    /// Output reader thread, joined on a bounded timeout.
    reader: Option<JoinHandle<()>>,
    /// Signalled by the reader thread when it has stopped.
    reader_finished: Option<Receiver<()>>,
    /// Current size, tracked so the session can report it.
    size: TerminalSize,
    /// Exit status, once observed (cached; `try_wait` is not repeated).
    exit: Option<ExitInfo>,
    /// OS-level orphan backstop for this child (spec section 15).
    ///
    /// Released explicitly and first by [`Drop`] below, for the ordering reason
    /// documented there; the field's own `Drop` is the idempotent safety net.
    guard: ProcessGuard,
}

impl PtyProcess {
    /// Allocate a PTY, spawn `spawn` inside it, and start pumping output.
    ///
    /// `on_output` is called from a dedicated reader thread for every chunk of
    /// output, in order. It must not block for long: the thread is what drains
    /// the PTY, and a slow callback applies back-pressure to the agent.
    pub fn spawn(
        spawn: &ProcessSpawn,
        size: TerminalSize,
        on_output: impl Fn(&[u8]) + Send + 'static,
    ) -> Result<Self, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size.to_pty_size())
            .map_err(|error| PtyError::Open(error.to_string()))?;

        // Destructure so the slave side can be closed right after spawning: the
        // parent has no reason to hold the child's end of the PTY open, and on
        // Unix that is what lets the master reader see EOF when the child exits.
        let portable_pty::PtyPair { slave, master } = pair;

        let mut command = CommandBuilder::new(&spawn.program);
        command.args(&spawn.args);
        for (name, value) in &spawn.environment {
            // Overrides the inherited value; the application's own environment
            // is untouched (spec section 7).
            command.env(name, value);
        }
        command.cwd(&spawn.cwd);

        let mut child = slave.spawn_command(command).map_err(|error| PtyError::Spawn {
            program: spawn.program.display().to_string(),
            message: error.to_string(),
        })?;
        drop(slave);

        // Orphan backstop, attached first so the window in which the agent could
        // start something the guard does not cover is as small as possible (the
        // race that remains is documented in `process::guard`). It is created
        // before anything else here can fail, so every early return below also
        // releases the guard - which is what takes the process tree with it.
        //
        // Infallible by design: an unarmed guard is logged, never an error. The
        // graceful-then-forced stop remains the primary protection (spec section
        // 15), and no session may fail because a backstop is unavailable
        // (spec section 16).
        let guard = ProcessGuard::attach(child.as_ref(), Some(master.as_ref()));

        // From here on a live child exists, so every failure path must
        // terminate it: dropping a process handle does not stop the process on
        // any platform, and an orphaned agent is exactly what spec section 15
        // forbids.
        let mut reader = match master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                terminate_after_setup_failure(&mut child);
                return Err(PtyError::Reader(error.to_string()));
            }
        };
        let writer = match master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                terminate_after_setup_failure(&mut child);
                return Err(PtyError::Write(error.to_string()));
            }
        };

        let (finished_sender, finished_receiver) = mpsc::channel();
        let reader_thread = match thread::Builder::new()
            .name("pty-reader".to_string())
            .spawn(move || {
                let mut buffer = vec![0u8; READ_BUFFER_SIZE];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => on_output(&buffer[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            // A torn-down PTY is the normal way this ends.
                            log::debug!("pty output reader stopped: {error}");
                            break;
                        }
                    }
                }
                // Ignored: the receiver may already be gone (detached reader).
                let _ = finished_sender.send(());
            }) {
            Ok(handle) => handle,
            Err(error) => {
                terminate_after_setup_failure(&mut child);
                return Err(PtyError::Reader(error.to_string()));
            }
        };

        Ok(Self {
            master: Some(master),
            writer: Some(writer),
            child,
            reader: Some(reader_thread),
            reader_finished: Some(finished_receiver),
            size,
            exit: None,
            guard,
        })
    }

    /// The process id, when the platform reports one.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Whether this child is covered by the OS-level orphan backstop.
    ///
    /// `false` means the guard could not be armed for this session - the reason is
    /// logged by [`ProcessGuard::attach`] - so stopping the agent depends on this
    /// application staying alive to do it. Reported in the session start log so an
    /// unavailable backstop is visible instead of silent.
    pub fn is_orphan_protected(&self) -> bool {
        self.guard.is_armed()
    }

    /// Current terminal size.
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// Whether the child is still alive (reaps it if it already exited).
    pub fn is_alive(&mut self) -> Result<bool, PtyError> {
        Ok(self.poll_exit()?.is_none())
    }

    /// Send bytes to the agent's input (keystrokes, pasted text, escape
    /// sequences). Fails once the input has been closed.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| PtyError::Write("the agent process input is already closed".to_string()))?;
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| PtyError::Write(error.to_string()))
    }

    /// Send Ctrl-C to an interactive agent: the graceful half of "stop".
    pub fn interrupt(&mut self) -> Result<(), PtyError> {
        self.write(&[INTERRUPT])
    }

    /// Close the agent's input so a well-behaved CLI sees EOF and exits.
    ///
    /// The reader thread is left running: output produced while shutting down
    /// must still reach the terminal view (spec section 15).
    pub fn close_input(&mut self) {
        self.writer = None;
    }

    /// Tell the PTY its new geometry so full-screen TUIs reflow (spec section 11).
    pub fn resize(&mut self, size: TerminalSize) -> Result<(), PtyError> {
        if size == self.size {
            return Ok(());
        }
        let master = self
            .master
            .as_ref()
            .ok_or_else(|| PtyError::Resize("the pseudo-terminal is already closed".to_string()))?;
        master
            .resize(size.to_pty_size())
            .map_err(|error| PtyError::Resize(error.to_string()))?;
        self.size = size;
        Ok(())
    }

    /// Forcibly terminate the child (spec section 15: forced termination when
    /// graceful shutdown does not work).
    ///
    /// On Unix this is `portable-pty`'s escalating path (SIGHUP, a short grace
    /// period, then SIGKILL); on Windows it is `TerminateProcess`.
    pub fn kill(&mut self) -> Result<(), PtyError> {
        self.child
            .kill()
            .map_err(|error| PtyError::Kill(error.to_string()))?;
        // Give the platform a moment for the handle to become reapable, so a
        // following `try_wait` reports the exit code instead of `None`.
        let _ = self.wait_for_exit(EXIT_POLL_INTERVAL);
        Ok(())
    }

    /// Poll for exit without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitInfo>, PtyError> {
        self.poll_exit()
    }

    /// Wait up to `timeout` for the child to exit.
    ///
    /// Bounded on purpose: every caller in this crate must be able to promise
    /// that a stop cannot hang the app (spec section 15).
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<Option<ExitInfo>, PtyError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(exit) = self.poll_exit()? {
                return Ok(Some(exit));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            thread::sleep(EXIT_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
        }
    }

    /// Reap the child if it has finished, caching the result.
    fn poll_exit(&mut self) -> Result<Option<ExitInfo>, PtyError> {
        if let Some(exit) = &self.exit {
            return Ok(Some(exit.clone()));
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let exit = ExitInfo::new(status.exit_code(), status.signal().map(str::to_string));
                self.exit = Some(exit.clone());
                Ok(Some(exit))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(PtyError::Wait(error.to_string())),
        }
    }
}

/// Terminate a child whose PTY could not be finished setting up.
///
/// Dropping a process handle does not stop the process (neither
/// `std::process::Child` nor `WinChild` kill on drop), so a failure *after* a
/// successful spawn has to kill explicitly or the agent stays behind with no
/// PTY attached (spec section 15: avoid orphaned agent processes).
fn terminate_after_setup_failure(child: &mut Box<dyn Child + Send + Sync>) {
    if let Err(error) = child.kill() {
        log::warn!("could not terminate a partially started agent process: {error}");
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        // The OS-level guard is released first, while the child is still alive:
        // on Unix the guard signals the child's *process group*, and it refuses to
        // signal a group whose leader has been reaped (see `process::guard`), so
        // releasing it after the kill below would make it a no-op. On Windows the
        // release closes the job object, which terminates the whole tree - child
        // included - and the kill below then covers a guard that was never armed.
        self.guard.terminate_tree();

        // Never leave an agent behind: a dropped PTY means the session is gone
        // (spec section 15, "avoid orphaned Claude Code processes").
        let _ = self.child.kill();
        self.writer = None;
        // Closing the master releases the pseudo-console, which is what makes a
        // reader parked inside the OS return.
        self.master = None;
        self.join_reader(READER_SHUTDOWN_TIMEOUT);
        // The guard is already released; its own `Drop` (running last, after this
        // body) is idempotent and covers the case where `terminate_tree` above is
        // ever removed. None of this is what the user sees when they press Stop -
        // that is the graceful-then-forced stop in `sessions`, which this only
        // backs up.
    }
}

impl PtyProcess {
    /// Join the reader thread, giving up after `timeout` so a caller can never
    /// be blocked by a PTY the OS has not finished tearing down.
    fn join_reader(&mut self, timeout: Duration) {
        let (Some(reader), Some(finished)) = (self.reader.take(), self.reader_finished.take())
        else {
            return;
        };
        match finished.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = reader.join();
            }
            Err(RecvTimeoutError::Timeout) => {
                log::debug!("pty output reader did not finish within {timeout:?}; detaching");
            }
        }
    }
}

impl std::fmt::Debug for PtyProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PtyProcess")
            .field("process_id", &self.process_id())
            .field("size", &self.size)
            .field("exit", &self.exit)
            .field("input_closed", &self.writer.is_none())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testing::{echo_command, interactive_shell_command};

    /// Every wait in these tests is bounded by this, so a broken PTY fails the
    /// suite instead of hanging it.
    const TIMEOUT: Duration = Duration::from_secs(15);

    /// Device Status Report request (`ESC [6n`): "where is the cursor?".
    const CURSOR_QUERY: &str = "\u{1b}[6n";

    /// The reply a terminal sends: cursor is at row 1, column 1.
    const CURSOR_REPORT: &[u8] = b"\x1b[1;1R";

    /// Read output until `predicate` matches, the stream ends, or `deadline`
    /// passes - and act as the barest possible terminal while doing it.
    ///
    /// Windows ConPTY opens by asking the attached terminal for the cursor
    /// position and holds the session's output until it gets an answer, so a
    /// test with no terminal emulator must answer it (`xterm.js` does this in
    /// the real app). Everything else is passed straight through.
    fn wait_for_output(
        pty: &mut PtyProcess,
        receiver: &Receiver<Vec<u8>>,
        deadline: Duration,
        predicate: impl Fn(&str) -> bool,
    ) -> String {
        let mut collected: Vec<u8> = Vec::new();
        let stop_at = Instant::now() + deadline;
        while Instant::now() < stop_at {
            let chunk = match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) => Some(chunk),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if let Some(chunk) = chunk {
                collected.extend_from_slice(&chunk);
                let text = String::from_utf8_lossy(&collected);
                if text.contains(CURSOR_QUERY) {
                    let _ = pty.write(CURSOR_REPORT);
                }
            }
            if predicate(&String::from_utf8_lossy(&collected)) {
                break;
            }
        }
        String::from_utf8_lossy(&collected).into_owned()
    }

    #[test]
    fn terminal_size_is_clamped_and_defaults_to_80x24() {
        assert_eq!(TerminalSize::default().cols(), 80);
        assert_eq!(TerminalSize::default().rows(), 24);

        // A frontend that reports 0 (a hidden or not-yet-laid-out terminal) must
        // not produce a zero-sized PTY.
        let zero = TerminalSize::from_columns_rows(0, 0);
        assert_eq!((zero.cols(), zero.rows()), (1, 1));

        let huge = TerminalSize::from_columns_rows(u32::MAX, 100_000);
        assert_eq!((huge.cols(), huge.rows()), (MAX_DIMENSION, MAX_DIMENSION));

        let exact = TerminalSize::from_columns_rows(120, 40);
        assert_eq!((exact.cols(), exact.rows()), (120, 40));
    }

    #[test]
    fn output_round_trips_through_a_real_pty_and_the_child_exits() {
        // The end-to-end check that a real process runs inside a real PTY and
        // its output reaches the callback (spec section 11).
        let token = format!("PTY-ROUND-TRIP-{}", std::process::id());
        let (program, args) = echo_command(&token);
        let spawn = ProcessSpawn::new(program, args, std::env::temp_dir(), Vec::new());

        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let mut pty = PtyProcess::spawn(&spawn, TerminalSize::default(), move |chunk| {
            // The receiver is gone once the test ends; that is not a failure.
            let _ = sender.send(chunk.to_vec());
        })
        .expect("spawn a real process in a PTY");

        let output = wait_for_output(&mut pty, &receiver, TIMEOUT, |text| text.contains(&token));
        assert!(
            output.contains(&token),
            "the PTY round trip produced no expected output; got: {output:?}"
        );

        // The echo command is short-lived and must be reaped with a clean status.
        let exit = pty
            .wait_for_exit(TIMEOUT)
            .expect("polling the child must succeed")
            .expect("the echo command must exit on its own");
        assert_eq!(exit.code, Some(0), "unexpected exit: {exit:?}");
        assert!(exit.success);
        assert!(!pty.is_alive().expect("reap the child"));
        // The exit status is cached, so a second wait is immediate and stable.
        assert_eq!(pty.wait_for_exit(TIMEOUT).unwrap(), Some(exit));
        assert!(pty.process_id().is_some());
    }

    #[test]
    fn input_reaches_the_child_and_forced_kill_ends_it() {
        // An interactive shell stands in for an interactive agent CLI: it stays
        // alive until it is stopped, which is what a session needs.
        let (program, args) = interactive_shell_command();
        let spawn = ProcessSpawn::new(program, args, std::env::temp_dir(), Vec::new());

        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let mut pty = PtyProcess::spawn(&spawn, TerminalSize::new(100, 30), move |chunk| {
            let _ = sender.send(chunk.to_vec());
        })
        .expect("spawn an interactive shell in a PTY");

        assert!(pty.is_alive().expect("poll the child"));
        assert_eq!(pty.size(), TerminalSize::new(100, 30));

        // Keystrokes must reach the child: the shell runs what we type and its
        // output comes back through the reader.
        let token = format!("INPUT-{}", std::process::id());
        pty.write(format!("echo {token}\r\n").as_bytes())
            .expect("write to the PTY");
        let output = wait_for_output(&mut pty, &receiver, TIMEOUT, |text| text.contains(&token));
        assert!(
            output.contains(&token),
            "input did not reach the child; got: {output:?}"
        );

        // A resize must be accepted while the process runs, and be idempotent.
        pty.resize(TerminalSize::new(120, 40)).expect("resize the PTY");
        assert_eq!(pty.size(), TerminalSize::new(120, 40));
        pty.resize(TerminalSize::new(120, 40)).expect("resize is idempotent");

        // Graceful stop: close the input, then force what is left.
        pty.close_input();
        let graceful = pty
            .wait_for_exit(Duration::from_millis(250))
            .expect("poll the child");
        if graceful.is_none() {
            pty.kill().expect("force-kill the child");
        }
        let exit = pty
            .wait_for_exit(TIMEOUT)
            .expect("poll the child")
            .expect("a stopped process must report an exit");
        assert!(!pty.is_alive().expect("reap the child"));

        // Either it saw EOF and exited cleanly (Unix shells), or it was killed.
        // Both are a valid, bounded stop; only the code differs by platform.
        assert!(exit.code.is_some(), "unexpected exit: {exit:?}");

        // Input is closed for good.
        assert!(pty.write(b"echo nope\r\n").is_err());
    }

    /// The wiring check for orphan prevention (spec section 15): the process a
    /// session really runs is covered by the OS-level guard, not only the
    /// hand-made child in `process::guard`'s own tests.
    ///
    /// Windows only: the Unix half of the guard signals a process group, which
    /// cannot be exercised here (see IMPLEMENTATION_PROGRESS.md). This spawns and
    /// stops one process of its own and touches nothing else.
    #[cfg(windows)]
    #[test]
    fn the_agent_process_a_session_spawns_is_covered_by_its_guard() {
        let (program, args) = interactive_shell_command();
        let spawn = ProcessSpawn::new(program, args, std::env::temp_dir(), Vec::new());
        let (sender, _receiver) = mpsc::channel::<Vec<u8>>();

        let mut pty = PtyProcess::spawn(&spawn, TerminalSize::default(), move |chunk| {
            let _ = sender.send(chunk.to_vec());
        })
        .expect("spawn an interactive shell in a PTY");

        let process_id = pty.process_id().expect("the platform must report a pid");
        assert!(
            pty.guard.is_armed(),
            "a spawned agent must be under the OS-level orphan backstop"
        );
        assert!(
            pty.guard.contains_process_id(process_id),
            "the PTY child must be a member of its guard's job object"
        );

        // The guard changes nothing about stopping: graceful first, then forced.
        pty.close_input();
        if pty
            .wait_for_exit(Duration::from_millis(250))
            .expect("poll the child")
            .is_none()
        {
            pty.kill().expect("force-kill the child");
        }
        let exit = pty
            .wait_for_exit(TIMEOUT)
            .expect("poll the child")
            .expect("a stopped process must report an exit");
        assert!(exit.code.is_some(), "unexpected exit: {exit:?}");
    }

    #[test]
    fn spawning_a_path_that_is_not_an_executable_reports_a_clear_error() {
        // Spec section 16: "Invalid executable". A path that exists but cannot
        // be launched is a different user mistake from a missing one (a stale
        // installer path, a script without its interpreter), and it must be
        // reported just as clearly: the program is named and the platform's own
        // message is kept, with no environment value alongside it.
        use crate::persistence::test_support::TempDir;

        let secret = "SK-SPAWN-ERROR-MUST-NOT-LEAK";
        let directory = TempDir::new("pty-invalid-executable");

        let not_a_program = directory.join("claude.txt");
        std::fs::write(&not_a_program, b"#!/bin/sh\nnot actually the CLI\n").unwrap();

        // (a) A regular file that is not a program, and (b) a directory used as
        // one: both are "exists, cannot be started".
        for program in [not_a_program.clone(), directory.path().to_path_buf()] {
            let spawn = ProcessSpawn::new(
                &program,
                Vec::new(),
                std::env::temp_dir(),
                vec![("ANTHROPIC_AUTH_TOKEN".to_string(), secret.to_string())],
            );

            let error = PtyProcess::spawn(&spawn, TerminalSize::default(), |_chunk| {})
                .expect_err("an unstartable path must not spawn");
            match &error {
                PtyError::Spawn { program: named, message } => {
                    assert_eq!(
                        named,
                        &program.display().to_string(),
                        "the error must name the program that could not start"
                    );
                    assert!(!message.is_empty(), "the platform detail must be kept");
                }
                other => panic!("unexpected error for {}: {other:?}", program.display()),
            }
            let rendered = error.to_string();
            assert!(!rendered.contains(secret), "leaked: {rendered}");
            assert!(!rendered.contains("ANTHROPIC_AUTH_TOKEN"), "leaked: {rendered}");
        }
    }

    #[test]
    fn spawning_a_missing_program_reports_a_clear_error_without_the_environment() {
        // A distinctive value stands in for a session credential: a spawn
        // failure must not echo the environment it was given (spec section 17).
        let secret = "SK-SPAWN-ERROR-MUST-NOT-LEAK";
        let spawn = ProcessSpawn::new(
            if cfg!(windows) {
                "definitely-not-a-real-program-xyz.exe"
            } else {
                "definitely-not-a-real-program-xyz"
            },
            Vec::new(),
            std::env::temp_dir(),
            vec![("ANTHROPIC_AUTH_TOKEN".to_string(), secret.to_string())],
        );

        let error = PtyProcess::spawn(&spawn, TerminalSize::default(), |_chunk| {})
            .expect_err("a missing program must not spawn");
        match &error {
            PtyError::Spawn { program, message } => {
                assert!(program.contains("definitely-not-a-real-program-xyz"));
                assert!(!message.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let rendered = error.to_string();
        assert!(!rendered.contains(secret), "leaked: {rendered}");
        assert!(!rendered.contains("ANTHROPIC_AUTH_TOKEN"), "leaked: {rendered}");
    }
}
