//! Executable discovery shared by every agent adapter (spec sections 6, 18).
//!
//! Every supported agent is a CLI installed somewhere on the user's `PATH`
//! (`claude`, `codex`, ...), so discovery is one `which`-style lookup rather
//! than a per-agent guess at a fixed install location. It lives here, next to
//! the adapters that use it, so adding an agent never means re-implementing it.
//!
//! Platform differences (spec section 18): on Windows a CLI may be a `.exe`, a
//! `.cmd`, or a `.bat` shim (npm global installs), so the common `PATHEXT`
//! extensions are tried; on Unix the candidate must be a regular file with an
//! executable bit.

use std::env;
use std::path::PathBuf;

#[cfg(not(windows))]
use std::path::Path;

/// `which`-style PATH lookup, implemented with std only.
///
/// Returns the first candidate that exists on `PATH`, or `None` when the agent
/// is not installed - which is what an adapter turns into
/// [`crate::agents::AgentError::NotInstalled`] (spec section 16).
pub(crate) fn find_in_path(executable: &str) -> Option<PathBuf> {
    find_in_path_with(&env::var_os("PATH")?, executable)
}

/// Extensions a Windows CLI may use, in preference order.
///
/// This mirrors the common `PATHEXT` order and deliberately tries the bare name
/// **last**: npm's global install ships `claude` (a POSIX shell script), and
/// `claude.cmd` alongside it, and `CreateProcessW` cannot launch the script.
/// Preferring the real Windows entry point is what makes discovery produce a
/// path that can actually be spawned (spec sections 6, 18). `.ps1` shims are
/// skipped on purpose: they need PowerShell to interpret them.
#[cfg(windows)]
const WINDOWS_EXTENSIONS: [&str; 5] = [".exe", ".com", ".cmd", ".bat", ""];

/// PATH lookup over an explicit `PATH` value.
///
/// Split out from [`find_in_path`] so executable discovery - including the
/// platform differences above - is unit-testable without touching the test
/// process's own `PATH`.
#[cfg(windows)]
pub(crate) fn find_in_path_with(path_var: &std::ffi::OsStr, executable: &str) -> Option<PathBuf> {
    for directory in env::split_paths(path_var) {
        for extension in WINDOWS_EXTENSIONS {
            let candidate = directory.join(format!("{executable}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// See the Windows counterpart above.
#[cfg(not(windows))]
pub(crate) fn find_in_path_with(path_var: &std::ffi::OsStr, executable: &str) -> Option<PathBuf> {
    for directory in env::split_paths(path_var) {
        let candidate = directory.join(executable);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::TempDir;
    use std::ffi::OsString;

    #[test]
    #[cfg(unix)]
    fn finds_a_known_unix_binary() {
        // `sh` exists on every supported Unix platform.
        assert!(find_in_path("sh").is_some());
    }

    #[test]
    #[cfg(unix)]
    fn misses_unknown_binary() {
        assert!(find_in_path("definitely-not-a-real-cli-xyz").is_none());
    }

    #[test]
    fn executable_discovery_searches_the_given_path_value() {
        // Executable discovery without touching the test process's PATH.
        let directory = TempDir::new("agent-path");
        let name = if cfg!(windows) { "codex.cmd" } else { "codex" };
        let candidate = directory.join(name);
        std::fs::write(&candidate, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let path_var = OsString::from(directory.path().as_os_str());
        assert_eq!(find_in_path_with(&path_var, "codex"), Some(candidate));
        assert_eq!(find_in_path_with(&path_var, "definitely-not-a-real-cli"), None);

        // A non-executable file on Unix must not be treated as the CLI.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(find_in_path_with(&path_var, "codex"), None);
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_discovery_prefers_a_spawnable_entry_point_over_the_bare_script() {
        // npm's global install lays down `claude` (a POSIX shell script),
        // `claude.cmd` and `claude.ps1`. Only the `.cmd` can be launched by
        // `CreateProcessW`, so discovery must not return the bare script.
        let directory = TempDir::new("agent-path-windows");
        let script = directory.join("claude");
        let shim = directory.join("claude.cmd");
        std::fs::write(&script, b"#!/bin/sh\nexec node cli.js \"$@\"\n").unwrap();
        std::fs::write(&shim, b"@echo off\r\nnode cli.js %*\r\n").unwrap();

        let path_var = OsString::from(directory.path().as_os_str());
        assert_eq!(find_in_path_with(&path_var, "claude"), Some(shim));

        // A real .exe still wins over a .cmd shim.
        let executable = directory.join("claude.exe");
        std::fs::write(&executable, b"MZ").unwrap();
        assert_eq!(find_in_path_with(&path_var, "claude"), Some(executable));
    }
}
