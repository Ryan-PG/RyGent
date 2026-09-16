//! OS-level backstop against orphaned agent processes (spec section 15:
//! "avoid orphaned Claude Code processes when the application exits").
//!
//! The primary stop path is unchanged and is still the one that runs in
//! practice: [`crate::sessions`] stops an agent gracefully (Ctrl-C, then EOF on
//! its input), waits out the grace period, and only then terminates it forcibly;
//! [`crate::pty::PtyProcess`] additionally kills its child when it is dropped.
//! **None of that can run when this application is hard-killed** - Task
//! Manager's "End task", `SIGKILL`, a crash, or a power loss execute no code in
//! this process - and that is the case this module covers. It is a backstop, not
//! a replacement, and a guard that cannot be armed never fails a session
//! (spec section 16): the reason is logged and the session starts anyway.
//!
//! # Windows
//!
//! One Job Object per spawned child, created with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and the child is assigned to it. From
//! then on the kernel is the guarantor: when the job's last handle closes - and
//! the kernel closes it itself when this process dies, however it dies - every
//! process still in the job is terminated. No code here has to run for that,
//! which is precisely what a hard kill otherwise leaves us unable to do.
//!
//! The process handle used for the assignment comes from
//! `portable_pty::Child::as_raw_handle`, i.e. the very handle `CreateProcessW`
//! produced for the child: no `OpenProcess` round trip, therefore no pid-reuse
//! window and no guess at access rights. portable-pty does not document that
//! method as a stable part of its surface, so `OpenProcess` by pid
//! (`PROCESS_SET_QUOTA | PROCESS_TERMINATE`, the rights `AssignProcessToJobObject`
//! requires) is kept as the fallback for a version that exposes no handle.
//!
//! # Unix
//!
//! `portable-pty` already calls `setsid()` in the child before `exec`
//! (verified in `portable-pty-0.9.0/src/unix.rs`), so every agent is the session
//! and process group leader of its own group with the PTY as its controlling
//! terminal. Nothing here adds a second mechanism for that: the guard reads the
//! group back through `MasterPty::process_group_leader` - the one process-group
//! accessor portable-pty exposes - and falls back to `getpgid(child)` when the
//! child has not taken the terminal over yet. On release it signals the **whole
//! group**: `SIGTERM`, a short bounded wait, then `SIGKILL`, so a grandchild
//! that outlived the CLI is not left behind.
//!
//! Unlike the Windows job, this is not a hard-kill backstop: `SIGKILL` runs no
//! code here either, so a `SIGKILL`ed application still leaves its agents.
//! Closing that gap needs a supervisor process outside this one (recorded in
//! IMPLEMENTATION_PROGRESS.md).
//!
//! # Known limits, deliberately
//!
//! - **Race between spawn and assignment.** `portable-pty` creates the child
//!   with `CreateProcessW` and *not* `CREATE_SUSPENDED` (verified in its
//!   source), so the agent runs for the instructions between the spawn
//!   returning and the `AssignProcessToJobObject` below. A grandchild started
//!   in that window is outside the job and would survive a hard kill. Closing
//!   the window needs `CREATE_SUSPENDED` plus a resume, which only the spawner
//!   controls; the graceful and forced stop paths still cover that grandchild.
//! - **Nested jobs.** Assignment fails if this process itself already runs in a
//!   job that does not permit it - Windows 7 and earlier cannot nest jobs at
//!   all, Windows 8+ can. A failure is logged as a warning and the session
//!   starts anyway: orphan protection is a backstop, never a reason to refuse a
//!   session.
//! - **One job per session, not one for the application.** A guard lives exactly
//!   as long as the PTY it protects, so a stopped session releases its job
//!   immediately instead of holding every agent it ever started.
//! - **Process ids can be recycled.** The Unix guard therefore signals the group
//!   only while the group leader is still present: an unreaped child (including a
//!   zombie) still owns its pid, so no other process can be leading that group,
//!   whereas signalling a group whose leader has already been reaped could hit an
//!   unrelated process. Members of a reaped child's group are then covered by the
//!   primary stop path, or not at all.
//! - **The Unix guard is not a hard-kill backstop** (see above), and it does not
//!   reach an agent that puts itself in a new session (`setsid()`), which leaves
//!   both the group and the PTY.
//! - **Only PTY-spawned processes are guarded.** [`PtyProcess`](crate::pty::PtyProcess)
//!   is the only place in this crate that creates a process, so that is where the
//!   guard is attached; nothing else in the application spawns an agent.

use std::fmt;

use portable_pty::{Child, MasterPty};

#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows::core::{BOOL, PCWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

/// How long the Unix backstop waits after `SIGTERM` before it uses `SIGKILL`.
///
/// Short on purpose: releasing the guard already means the session is gone, and in
/// the normal stop case the graceful path has given the agent its grace period
/// first - so this is only the last shove for whatever ignored both Ctrl-C and EOF.
#[cfg(unix)]
const GROUP_TERM_GRACE: Duration = Duration::from_millis(500);

/// How often the Unix backstop re-checks whether the process group is empty.
#[cfg(unix)]
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The OS resource that keeps one agent's process tree reachable (spec section 15).
///
/// Created by [`ProcessGuard::attach`] immediately after a child is spawned, and
/// held by the [`PtyProcess`](crate::pty::PtyProcess) that owns that child, so its
/// lifetime *is* the session's lifetime. Releasing it - through
/// [`ProcessGuard::terminate_tree`] or by dropping it - gives up the OS resource:
///
/// - Windows: closing the job object handle, which - because of
///   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` - terminates every process still in the
///   job, including when the release happens because this process died.
/// - Unix: `SIGTERM` to the child's process group, then `SIGKILL` after the
///   guard's grace period.
///
/// A guard that could not be armed is not an error and has no `Result` shape for
/// the caller: it reports `false` from [`ProcessGuard::is_armed`] and releasing it
/// does nothing, so the call site stays a single infallible line.
pub struct ProcessGuard {
    /// Windows: the job object, or `None` when orphan protection is unavailable.
    ///
    /// Dropping this field is the kill (see the job object's own documentation).
    #[cfg(windows)]
    job: Option<JobObject>,

    /// Unix: the child's process group, or `None` when it could not be read.
    #[cfg(unix)]
    process_group: Option<libc::pid_t>,
}

impl ProcessGuard {
    /// Place the just-spawned `child` under OS-level orphan protection.
    ///
    /// `master` is that child's PTY master. Unix needs it (it is where
    /// `portable-pty` exposes the child's process group); Windows ignores it.
    ///
    /// Infallible by design: a guard that cannot be armed logs why and reports
    /// [`is_armed`](ProcessGuard::is_armed) `false`. Orphan protection is a
    /// backstop - failing a start over it would trade a rare leak for a session
    /// the user cannot open (spec section 16).
    pub fn attach(child: &(dyn Child + Send + Sync), master: Option<&dyn MasterPty>) -> Self {
        #[cfg(windows)]
        let guard = {
            // A job object is attached to the process, not to its console, so the
            // PTY plays no part on Windows.
            let _ = master;
            Self::attach_to_job_object(child)
        };

        #[cfg(unix)]
        let guard = Self::attach_to_process_group(child, master);

        guard
    }

    /// Whether this guard actually holds an OS resource.
    ///
    /// `false` means the backstop is unavailable for this session (the reason is
    /// in the log) and the graceful-then-forced stop is the only protection.
    pub fn is_armed(&self) -> bool {
        #[cfg(windows)]
        let armed = self.job.is_some();

        #[cfg(unix)]
        let armed = self.process_group.is_some();

        armed
    }

    /// Release the guard now, terminating the agent's process tree.
    ///
    /// Idempotent and infallible, so both `Drop` and a caller that needs a
    /// particular release *order* can use it. The order matters on Unix: the guard
    /// signals the child's process group, and it refuses to do that once the child
    /// has been reaped (see the pid-recycling limit in the module docs), so a
    /// caller that is about to kill and reap the child should release the guard
    /// first - which is what [`PtyProcess::drop`](crate::pty::PtyProcess) does.
    ///
    /// Stopping a session on purpose does **not** call this: the
    /// graceful-then-forced stop in [`crate::sessions`] is the primary path, and it
    /// targets the agent rather than the agent's whole tree.
    pub fn terminate_tree(&mut self) {
        // Windows: release the job object, which is the kill.
        #[cfg(windows)]
        self.release_job_object();

        // Unix: signal the child's process group.
        #[cfg(unix)]
        self.terminate_process_group();
    }
}

#[cfg(windows)]
impl ProcessGuard {
    /// Create a kill-on-close job object and put `child` in it.
    fn attach_to_job_object(child: &(dyn Child + Send + Sync)) -> Self {
        let job = match JobObject::create_kill_on_close() {
            Ok(job) => job,
            Err(error) => {
                log::warn!(
                    "orphan protection unavailable: could not create a job object ({error}); \
                     the graceful-then-forced stop still applies"
                );
                return Self { job: None };
            }
        };

        // Preferred source: the handle `CreateProcessW` produced for this child.
        // Fallback: open one by pid, with exactly the rights assignment needs.
        // The two differ in who owns the handle, which is why `opened` is kept.
        let (process, opened) = match child.as_raw_handle() {
            Some(handle) => (HANDLE(handle), None),
            None => match child.process_id().and_then(open_process_for_assignment) {
                Some(handle) => (handle, Some(handle)),
                None => {
                    log::warn!(
                        "orphan protection unavailable: the agent process exposes no handle and \
                         could not be opened by pid; the graceful-then-forced stop still applies"
                    );
                    return Self { job: None };
                }
            },
        };

        let assigned = unsafe { AssignProcessToJobObject(job.handle(), process) };

        // Only a handle opened here may be closed here: the raw handle belongs to
        // `portable-pty`'s child and is closed when that child is.
        if let Some(handle) = opened {
            let _ = unsafe { CloseHandle(handle) };
        }

        match assigned {
            Ok(()) => {
                log::debug!("orphan protection armed: the agent is in a kill-on-close job object");
                Self { job: Some(job) }
            }
            Err(error) => {
                // See the nested-job limit in the module docs: on an older Windows,
                // or inside a restrictive job, this is expected and harmless.
                log::warn!(
                    "orphan protection unavailable: the agent could not be assigned to a job \
                     object ({error}); the graceful-then-forced stop still applies"
                );
                Self { job: None }
            }
        }
    }

    /// Whether `process_id` is a member of this guard's job object.
    ///
    /// Diagnostics and tests only - the application never needs to ask. Reports
    /// `false` rather than failing when there is no job, when the process is
    /// already gone, or when it cannot be queried.
    pub fn contains_process_id(&self, process_id: u32) -> bool {
        let Some(job) = self.job.as_ref() else {
            return false;
        };
        let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        {
            Ok(process) => process,
            Err(_error) => return false,
        };
        let mut in_job = BOOL(0);
        let queried = unsafe { IsProcessInJob(process, Some(job.handle()), &mut in_job) }.is_ok();
        let _ = unsafe { CloseHandle(process) };
        queried && in_job.as_bool()
    }

    /// Release the job object, which terminates what is still in it.
    fn release_job_object(&mut self) {
        // The close *is* the Windows backstop: with KILL_ON_JOB_CLOSE the kernel
        // terminates whatever is still in the job, however this release came about.
        self.job = None;
    }
}

/// A job object handle, owned by exactly one [`ProcessGuard`].
///
/// Dropping it closes the handle, and closing the handle is what terminates the
/// job: the object is created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the
/// kernel kills every process still in it when the last handle goes away - which
/// is also what the kernel does for us when this process dies without running any
/// code of ours.
#[cfg(windows)]
struct JobObject(HANDLE);

// SAFETY: a job object handle is a kernel handle, not a pointer into this
// process's address space. `HANDLE` wraps a raw pointer, so the compiler cannot
// see that on its own and these assertions are needed. They hold because the
// handle is owned exclusively (created in `create_kill_on_close`, closed exactly
// once in `Drop`) and the `Win32` job APIs are callable from any thread on a
// handle without external synchronisation.
#[cfg(windows)]
unsafe impl Send for JobObject {}

// SAFETY: as above; every method takes `&self` and touches no memory of this
// process, so sharing the handle across threads is sound.
#[cfg(windows)]
unsafe impl Sync for JobObject {}

#[cfg(windows)]
impl JobObject {
    /// Create a job object that kills its members when its last handle closes.
    fn create_kill_on_close() -> windows::core::Result<Self> {
        // An unnamed, uninheritable job: nothing outside this process can reach it.
        let job = Self(unsafe { CreateJobObjectW(None, PCWSTR::null())? });

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }

        Ok(job)
    }

    fn handle(&self) -> HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        // The close itself is the kill - see the type's documentation. A failure
        // here (an already-invalid handle) is not actionable and not worth a log
        // line while the application is shutting down.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Open the process with the access rights `AssignProcessToJobObject` requires.
///
/// The fallback path of [`ProcessGuard::attach_to_job_object`], for a
/// `portable-pty` that exposes no raw child handle. The caller owns the returned
/// handle and must close it.
#[cfg(windows)]
fn open_process_for_assignment(process_id: u32) -> Option<HANDLE> {
    match unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, process_id) } {
        Ok(handle) => Some(handle),
        Err(error) => {
            log::warn!("could not open the agent process {process_id} for job assignment: {error}");
            None
        }
    }
}

#[cfg(unix)]
impl ProcessGuard {
    /// Record the process group `portable-pty` already put `child` in.
    fn attach_to_process_group(
        child: &(dyn Child + Send + Sync),
        master: Option<&dyn MasterPty>,
    ) -> Self {
        // The PTY knows the group authoritatively (it is the group the terminal
        // considers foreground); the child's own group is the fallback for the
        // moment before it has taken the terminal over. No second mechanism: the
        // child is already a session leader because `portable-pty` calls
        // `setsid()` before `exec`.
        let group = master
            .and_then(|master| master.process_group_leader())
            .or_else(|| child.process_id().and_then(process_group_of));

        match group {
            Some(group) if group > 0 && Some(group) != process_group_of(0) => {
                log::debug!("orphan protection armed: the agent owns process group {group}");
                Self {
                    process_group: Some(group),
                }
            }
            // Refusing to arm is deliberate: signalling the application's own
            // process group on release would kill this application and whatever
            // else shares its shell, which is worse than the leak it prevents.
            Some(group) => {
                log::warn!(
                    "orphan protection unavailable: the agent appears to share process group \
                     {group} with the application"
                );
                Self { process_group: None }
            }
            None => {
                log::warn!(
                    "orphan protection unavailable: the agent's process group could not be read; \
                     the graceful-then-forced stop still applies"
                );
                Self { process_group: None }
            }
        }
    }

    /// Signal the child's process group: `SIGTERM`, then `SIGKILL`.
    fn terminate_process_group(&mut self) {
        let Some(group) = self.process_group.take() else {
            return;
        };
        // See the pid-recycling limit in the module docs: the group is only
        // signalled while its leader (the child, whose pid *is* the group id
        // because portable-pty made it a session leader) is still present.
        if !process_exists(group) {
            log::debug!(
                "orphan protection: process group {group} has no leader left; not signalling it"
            );
            return;
        }

        signal_group(group, libc::SIGTERM);
        let deadline = Instant::now() + GROUP_TERM_GRACE;
        while Instant::now() < deadline {
            if !group_present(group) {
                return;
            }
            thread::sleep(GROUP_POLL_INTERVAL);
        }
        log::debug!("process group {group} outlived SIGTERM; killing it");
        signal_group(group, libc::SIGKILL);
    }
}

/// The process group a process belongs to, when the kernel reports it.
#[cfg(unix)]
fn process_group_of(process_id: u32) -> Option<libc::pid_t> {
    let group = unsafe { libc::getpgid(process_id as libc::pid_t) };
    (group > 0).then_some(group)
}

/// Whether a process still exists.
///
/// A zombie counts as existing, which is what makes it safe to signal its group:
/// an unreaped process still owns its pid, so no other process can be leading
/// that group yet.
#[cfg(unix)]
fn process_exists(process_id: libc::pid_t) -> bool {
    // Signal 0 delivers nothing; it only runs the existence check. EPERM means
    // "it is there, but it is not ours to signal".
    unsafe { libc::kill(process_id, 0) == 0 }
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether any process is left in a process group.
#[cfg(unix)]
fn group_present(group: libc::pid_t) -> bool {
    // A negative pid means "the whole process group".
    unsafe { libc::kill(-group, 0) == 0 }
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Signal every process in a group, ignoring "there is nothing left to signal".
#[cfg(unix)]
fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    if unsafe { libc::kill(-group, signal) } == 0 {
        return;
    }
    let error = std::io::Error::last_os_error();
    // ESRCH is the normal, successful outcome when the group emptied itself.
    if error.raw_os_error() != Some(libc::ESRCH) {
        log::warn!("could not signal agent process group {group} with signal {signal}: {error}");
    }
}

impl Drop for ProcessGuard {
    /// The release a whole session's teardown goes through: a guard that is
    /// dropped without an explicit [`ProcessGuard::terminate_tree`] still takes the
    /// agent's process tree with it, which is what happens on every early return
    /// of [`PtyProcess::spawn`](crate::pty::PtyProcess::spawn).
    fn drop(&mut self) {
        self.terminate_tree();
    }
}

impl fmt::Debug for ProcessGuard {
    /// Prints no handles and no environment: a guard is described by what it
    /// protects, never by anything a session supplied (spec section 17).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessGuard")
            .field(
                "resource",
                &if cfg!(windows) { "job-object" } else { "process-group" },
            )
            .field("armed", &self.is_armed())
            .finish()
    }
}

/// Guard tests.
///
/// **Windows only.** The Unix half of the guard (process-group signalling) cannot
/// be exercised on the machine this was written on, and is deliberately left
/// untested rather than written blind; the gap is recorded in
/// IMPLEMENTATION_PROGRESS.md. Every process used here is spawned by the test
/// itself and killed on drop - no test in this suite signals a process it did not
/// create, and none of them touch a `claude` process the user happens to be
/// running.
#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::STILL_ACTIVE;
    use windows::Win32::System::JobObjects::{
        JobObjectBasicProcessIdList, QueryInformationJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    };
    use windows::Win32::System::Threading::{GetExitCodeProcess, TerminateProcess};

    /// Bound for every wait in these tests, so a guard that does not work fails
    /// the suite instead of hanging it.
    const DEATH_TIMEOUT: Duration = Duration::from_secs(10);

    /// How long an agent is given to start the process this test looks for.
    const INHERIT_TIMEOUT: Duration = Duration::from_secs(15);

    /// How often a test checks whether a process it spawned has died.
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// The largest process list the job query below will read back.
    const MAX_JOB_PROCESSES: usize = 64;

    /// A long-lived process a test spawned itself, killed and reaped on drop.
    ///
    /// Dropping it is what guarantees a failed assertion cannot leave a process
    /// behind.
    struct OwnedChild(std::process::Child);

    impl OwnedChild {
        /// Start `cmd.exe /k`, a shell that reads commands from the piped stdin.
        fn spawn() -> Self {
            let child = Command::new("cmd.exe")
                .arg("/k")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn cmd.exe");
            Self(child)
        }

        fn id(&self) -> u32 {
            self.0.id()
        }

        /// Whether the child is still running (reaping it if it already exited).
        fn is_alive(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }

        /// Wait, bounded, for the child to die.
        fn wait_until_dead(&mut self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if !self.is_alive() {
                    return true;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            false
        }

        /// Run one command line in the shell, the way an agent starts a helper.
        fn run(&mut self, command: &str) {
            use std::io::Write;

            let stdin = self
                .0
                .stdin
                .as_mut()
                .expect("the shell was started with a piped stdin");
            stdin
                .write_all(format!("{command}\r\n").as_bytes())
                .expect("write a command to the shell");
            stdin.flush().expect("flush the command");
        }

        /// The child in the shape [`ProcessGuard::attach`] takes, i.e. exactly the
        /// `portable_pty::Child` it is handed by the PTY spawn path.
        fn as_spawn(&self) -> &(dyn Child + Send + Sync) {
            &self.0
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A handle to a process a test found inside the job.
    ///
    /// The handle is kept open for as long as the test needs it: a handle refers
    /// to one process object, so neither the liveness check nor the cleanup below
    /// can ever act on a different process that later reused the pid.
    struct OwnedProcess(HANDLE);

    impl OwnedProcess {
        fn open(process_id: u32) -> Self {
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    false,
                    process_id,
                )
            }
            .expect("open a process the agent started");
            Self(handle)
        }

        fn is_alive(&self) -> bool {
            let mut exit_code = 0u32;
            unsafe { GetExitCodeProcess(self.0, &mut exit_code) }.is_ok()
                && exit_code as i32 == STILL_ACTIVE.0
        }

        /// Wait, bounded, for the process to exit.
        fn wait_until_dead(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if !self.is_alive() {
                    return true;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            false
        }
    }

    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            // Only this test's own grandchild, through the handle opened above.
            let _ = unsafe { TerminateProcess(self.0, 1) };
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// The pids the kernel reports as members of the guard's job.
    fn job_process_ids(guard: &ProcessGuard) -> Vec<u32> {
        let Some(job) = guard.job.as_ref() else {
            return Vec::new();
        };
        // `JOBOBJECT_BASIC_PROCESS_ID_LIST` ends in a variable-length array, so the
        // buffer is sized by hand: two header words plus the pid entries. A `usize`
        // vector rather than a byte one, so the struct's alignment is guaranteed.
        let mut buffer = vec![0usize; 2 + MAX_JOB_PROCESSES];
        let queried = unsafe {
            QueryInformationJobObject(
                Some(job.handle()),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr() as *mut core::ffi::c_void,
                (buffer.len() * std::mem::size_of::<usize>()) as u32,
                None,
            )
        };
        if queried.is_err() {
            return Vec::new();
        }

        let list = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
        let count = (list.NumberOfProcessIdsInList as usize).min(MAX_JOB_PROCESSES);
        unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) }
            .iter()
            .map(|process_id| *process_id as u32)
            .collect()
    }

    /// Wait, bounded, for a job member other than `agent`, i.e. for a process the
    /// agent started after it was assigned.
    fn wait_for_a_process_the_agent_started(
        guard: &ProcessGuard,
        agent: u32,
        timeout: Duration,
    ) -> Option<u32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(started) = job_process_ids(guard)
                .into_iter()
                .find(|process_id| *process_id != agent)
            {
                return Some(started);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        None
    }

    #[test]
    fn a_child_the_guard_covers_dies_when_the_guard_is_released() {
        let mut child = OwnedChild::spawn();
        let guard = ProcessGuard::attach(child.as_spawn(), None);

        assert!(
            guard.is_armed(),
            "a child of this test must be assignable to a job object"
        );
        assert!(
            guard.contains_process_id(child.id()),
            "the spawned child must be a member of the guard's job object"
        );
        assert!(child.is_alive(), "the child must be alive before the release");

        drop(guard);

        assert!(
            child.wait_until_dead(DEATH_TIMEOUT),
            "closing the job object handle must terminate the processes in it \
             (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)"
        );
    }

    /// The reason this is a job object and not one more `TerminateProcess`: a
    /// process the agent starts *after* the assignment inherits job membership,
    /// so releasing the guard reaches the agent's own children too - which the
    /// direct kill in `PtyProcess::drop` cannot do.
    #[test]
    fn a_process_the_agent_starts_inherits_the_job_and_dies_with_it() {
        let mut child = OwnedChild::spawn();
        let guard = ProcessGuard::attach(child.as_spawn(), None);
        assert!(guard.is_armed());

        // Non-vacuous by construction: the job holds the agent and nothing else
        // before the agent starts anything.
        assert_eq!(
            job_process_ids(&guard),
            vec![child.id()],
            "a fresh job must hold exactly the agent it was armed for"
        );

        // The agent starts a helper of its own, the way an agent CLI starts a
        // language server or a tool.
        child.run("start /b cmd /k");

        let inherited = wait_for_a_process_the_agent_started(&guard, child.id(), INHERIT_TIMEOUT)
            .expect("a process started by the agent must inherit the agent's job");
        let started = OwnedProcess::open(inherited);
        assert!(
            started.is_alive(),
            "the process the agent started must be running"
        );

        drop(guard);

        assert!(
            child.wait_until_dead(DEATH_TIMEOUT),
            "the agent itself must be terminated"
        );
        assert!(
            started.wait_until_dead(DEATH_TIMEOUT),
            "closing the job must also terminate the process the agent started: the guard is a \
             tree kill, not a second kill for the agent alone"
        );
    }

    /// The negative control: releasing a guard must not touch a process that was
    /// never assigned to it, and must not reach this test process itself.
    #[test]
    fn releasing_a_guard_leaves_processes_it_does_not_cover_alone() {
        let mut covered = OwnedChild::spawn();
        let mut bystander = OwnedChild::spawn();
        let guard = ProcessGuard::attach(covered.as_spawn(), None);
        assert!(guard.is_armed());

        assert!(
            !guard.contains_process_id(bystander.id()),
            "an unrelated process must not be reported as a job member"
        );
        assert!(
            !guard.contains_process_id(std::process::id()),
            "the application's own process must not end up in an agent's job"
        );

        drop(guard);

        assert!(
            covered.wait_until_dead(DEATH_TIMEOUT),
            "the covered child must be terminated"
        );
        assert!(
            bystander.is_alive(),
            "a process the guard does not cover must survive its release"
        );
    }

    /// Two guards are independent: releasing one releases only its own job.
    #[test]
    fn a_released_guard_does_not_end_another_guard_s_child() {
        let mut first = OwnedChild::spawn();
        let mut second = OwnedChild::spawn();
        let first_guard = ProcessGuard::attach(first.as_spawn(), None);
        let second_guard = ProcessGuard::attach(second.as_spawn(), None);
        assert!(first_guard.is_armed() && second_guard.is_armed());
        assert!(!first_guard.contains_process_id(second.id()));

        drop(first_guard);

        assert!(
            first.wait_until_dead(DEATH_TIMEOUT),
            "the first child must be terminated by its own guard"
        );
        assert!(second.is_alive(), "the second guard's child must survive");

        drop(second_guard);
        assert!(
            second.wait_until_dead(DEATH_TIMEOUT),
            "the second child must be terminated once its guard is released"
        );
    }

    /// A guard that could not be armed is inert, which is what makes "never fail
    /// a session over the backstop" safe.
    #[test]
    fn an_unarmed_guard_is_inert() {
        let guard = ProcessGuard { job: None };
        assert!(!guard.is_armed());
        assert!(!guard.contains_process_id(std::process::id()));

        let mut child = OwnedChild::spawn();
        drop(guard);
        assert!(
            child.is_alive(),
            "releasing a guard that was never armed must not touch any process"
        );
    }

    #[test]
    fn a_guard_describes_itself_without_revealing_anything() {
        let child = OwnedChild::spawn();
        let guard = ProcessGuard::attach(child.as_spawn(), None);

        let rendered = format!("{guard:?}");
        assert!(rendered.contains("ProcessGuard"));
        assert!(rendered.contains("job-object"));
        assert!(rendered.contains("armed: true"));
        assert!(
            !rendered.contains(&child.id().to_string()),
            "the description must not carry process ids: {rendered}"
        );
    }
}
