//! Shared construction of an agent session's spawn description.
//!
//! Every adapter answers the same three questions about a session start: is the
//! project folder usable as a working directory, does the session have its own
//! configuration directory, and what environment should the child process see.
//! Only the *last* one is agent-specific (a Claude Code session and a Codex
//! session are configured with different variables); the first two are checks
//! and bookkeeping every agent needs, so they live here rather than being copied
//! into each adapter.
//!
//! The result is a plain [`ProcessSpawn`], which is what makes an adapter
//! testable without a PTY and a PTY testable without an agent (spec section 23).

use std::path::Path;

use crate::process::ProcessSpawn;

use super::{AgentError, SpawnRequest};

/// Build the spawn description for one session.
///
/// `environment` is the agent-specific environment (see
/// [`crate::agents::AgentAdapter::build_environment`]). The session's
/// configuration directory is appended to it **last**, so a provider's extra
/// environment variables cannot redirect the session away from its own
/// configuration directory: config isolation (spec section 8) must not be
/// optional. `config_dir_variable` is the agent's name for that directory
/// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, ...).
///
/// Values in the returned description may contain secrets, so callers must never
/// log it (spec section 17). [`ProcessSpawn`]'s own `Debug` renders variable
/// names only, which is what makes the safe rendering the default one.
pub(crate) fn build_session_spawn(
    executable: &Path,
    args: &[String],
    request: &SpawnRequest,
    mut environment: Vec<(String, String)>,
    config_dir_variable: &str,
) -> Result<ProcessSpawn, AgentError> {
    // The project folder is the agent's working directory, so a missing or
    // non-directory path must fail the start attempt with a clear message
    // (spec section 16: "Project directory deleted").
    validate_project_directory(&request.project_path)?;

    // Per-session configuration directory (settings, session history, plugins).
    // Created here because it is only needed by the agent.
    std::fs::create_dir_all(&request.config_dir).map_err(|error| AgentError::ConfigDirectory {
        path: request.config_dir.display().to_string(),
        message: error.to_string(),
    })?;

    environment.push((
        config_dir_variable.to_string(),
        request.config_dir.display().to_string(),
    ));

    Ok(ProcessSpawn::new(
        executable.to_path_buf(),
        args.to_vec(),
        request.project_path.clone(),
        environment,
    ))
}

/// Check that the project folder exists, is a directory, and can be opened.
fn validate_project_directory(path: &Path) -> Result<(), AgentError> {
    if !path.exists() {
        return Err(AgentError::ProjectDirectoryMissing {
            path: path.display().to_string(),
        });
    }
    if !path.is_dir() {
        return Err(AgentError::ProjectDirectoryInvalid {
            path: path.display().to_string(),
        });
    }
    // ... and that it can actually be *opened*. `is_dir()` proves the path is a
    // directory, not that this process may use it; without this probe the
    // failure surfaces much later and much less clearly, from inside the spawn
    // (`CreateProcessW`'s "The directory name is invalid." or a bare `EACCES`),
    // which names neither the folder nor the fix (spec section 16: "Permission
    // errors").
    check_project_directory_access(path)
}

/// Check that the project folder can be opened, so a permission problem is
/// reported as itself (spec section 16) instead of as a spawn failure.
///
/// Opening the directory for reading is the cheapest faithful probe: on both
/// platforms that is where the OS applies the access check, and a session
/// cannot run against a folder whose contents the agent may not read anyway
/// (every supported agent reads the project). Only the first entry (if any) is
/// inspected; an empty project folder is perfectly valid.
pub(crate) fn check_project_directory_access(path: &Path) -> Result<(), AgentError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            // Touching the iterator forces the platform call that the open
            // itself may have deferred (`FindFirstFileW` / `opendir`).
            let _ = entries.next();
            Ok(())
        }
        Err(error) => Err(project_directory_access_error(path, &error)),
    }
}

/// Map an access failure on a project folder to its user-facing error.
///
/// Split out from the probe so the mapping - which is what the user reads - is
/// testable without a directory that is genuinely unreadable (spec section 23).
pub(crate) fn project_directory_access_error(path: &Path, error: &std::io::Error) -> AgentError {
    AgentError::ProjectDirectoryNotAccessible {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::TempDir;
    use crate::providers::{ProviderProfile, ResolvedProvider};

    /// A resolved provider with no extra variables, as the command layer builds
    /// one: a stored profile plus the key read from the secret store.
    fn resolved() -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            },
            Some("KEY_A".to_string()),
        )
    }

    fn request(project: &Path, config: &Path, provider: ResolvedProvider) -> SpawnRequest {
        SpawnRequest {
            project_path: project.to_path_buf(),
            config_dir: config.to_path_buf(),
            provider,
        }
    }

    #[test]
    fn the_shared_builder_sets_the_working_directory_and_the_config_variable() {
        let project = TempDir::new("spawn-shared-project");
        let config_root = TempDir::new("spawn-shared-config");
        let config_dir = config_root.join("session-1");

        let spawn = build_session_spawn(
            Path::new("codex"),
            &["--model".to_string(), "model-a".to_string()],
            &request(project.path(), &config_dir, resolved()),
            vec![("OPENAI_API_KEY".to_string(), "KEY_A".to_string())],
            "CODEX_HOME",
        )
        .expect("build a spawn description");

        assert_eq!(spawn.program, std::path::PathBuf::from("codex"));
        assert_eq!(spawn.args, vec!["--model".to_string(), "model-a".to_string()]);
        assert_eq!(spawn.cwd, project.path());
        assert_eq!(
            spawn.environment_value("CODEX_HOME"),
            Some(config_dir.display().to_string().as_str())
        );
        // The configuration directory was created for the session.
        assert!(config_dir.is_dir(), "the session config dir must exist");
        // The agent's own environment is preserved.
        assert_eq!(spawn.environment_value("OPENAI_API_KEY"), Some("KEY_A"));
        // The description must not print the credential (spec section 17).
        let rendered = format!("{spawn:?}");
        assert!(!rendered.contains("KEY_A"), "leaked: {rendered}");
    }

    #[test]
    fn the_config_directory_is_applied_last_so_an_extra_cannot_redirect_it() {
        // Config isolation is not optional (spec section 8): the variable is
        // appended after the agent's environment, so a provider profile cannot
        // point two sessions at the same configuration directory.
        let project = TempDir::new("spawn-shared-order-project");
        let config_root = TempDir::new("spawn-shared-order-config");
        let config_dir = config_root.join("session-1");

        let spawn = build_session_spawn(
            Path::new("codex"),
            &[],
            &request(project.path(), &config_dir, resolved()),
            vec![(
                "CODEX_HOME".to_string(),
                "C:\\shared\\codex-home".to_string(),
            )],
            "CODEX_HOME",
        )
        .expect("build a spawn description");

        assert_eq!(
            spawn.environment_value("CODEX_HOME"),
            Some(config_dir.display().to_string().as_str())
        );
    }

    #[test]
    fn a_deleted_project_folder_fails_the_spawn_description() {
        let config_root = TempDir::new("spawn-shared-missing-config");
        let missing = config_root.join("does-not-exist");

        let error = build_session_spawn(
            Path::new("codex"),
            &[],
            &request(&missing, &config_root.join("session-1"), resolved()),
            Vec::new(),
            "CODEX_HOME",
        )
        .expect_err("a missing project folder must fail");
        assert!(matches!(error, AgentError::ProjectDirectoryMissing { .. }));
        assert!(error.to_string().contains("does-not-exist"));
    }

    #[test]
    fn a_project_path_that_is_a_file_fails_the_spawn_description() {
        let project = TempDir::new("spawn-shared-file-project");
        let config_root = TempDir::new("spawn-shared-file-config");
        let file = project.join("a-file.txt");
        std::fs::write(&file, b"not a folder").unwrap();

        let error = build_session_spawn(
            Path::new("codex"),
            &[],
            &request(&file, &config_root.join("session-1"), resolved()),
            Vec::new(),
            "CODEX_HOME",
        )
        .expect_err("a file is not a project folder");
        assert!(matches!(error, AgentError::ProjectDirectoryInvalid { .. }));
    }

    #[test]
    fn a_config_directory_that_cannot_be_created_fails_the_spawn_description() {
        let project = TempDir::new("spawn-shared-blocked-project");
        let blocker = TempDir::new("spawn-shared-blocked-config");
        // A *file* where the config directory's parent needs to be.
        let blocked_parent = blocker.join("blocked");
        std::fs::write(&blocked_parent, b"not a folder").unwrap();

        let error = build_session_spawn(
            Path::new("codex"),
            &[],
            &request(
                project.path(),
                &blocked_parent.join("session-1"),
                resolved(),
            ),
            Vec::new(),
            "CODEX_HOME",
        )
        .expect_err("an unusable config directory must fail");
        assert!(matches!(error, AgentError::ConfigDirectory { .. }));
        // No credential may appear in the message (spec section 17).
        assert!(!error.to_string().contains("KEY_A"));
    }

    /// The access probe (spec section 16: "Permission errors", "Project
    /// directory deleted").
    ///
    /// A readable folder - including an empty one - must never be refused: the
    /// probe exists to turn an OS-level permission failure into a clear message,
    /// not to add a new way for a valid project to be rejected.
    #[test]
    fn a_readable_project_folder_passes_the_access_probe() {
        let empty = TempDir::new("spawn-probe-empty");
        assert!(
            check_project_directory_access(empty.path()).is_ok(),
            "an empty project folder is valid"
        );

        let populated = TempDir::new("spawn-probe-populated");
        std::fs::write(populated.join("main.rs"), b"fn main() {}").unwrap();
        std::fs::create_dir_all(populated.join("src")).unwrap();
        assert!(check_project_directory_access(populated.path()).is_ok());
    }

    /// The message a permission failure produces.
    ///
    /// The failing `read_dir` is the *same* call the probe makes, with the error
    /// kind the operating system raises for an unreadable directory
    /// (`EACCES`/`ERROR_ACCESS_DENIED` both map to `PermissionDenied`), so what
    /// is asserted here is the production mapping the user reads.
    #[test]
    fn a_project_folder_that_cannot_be_opened_reports_an_actionable_message() {
        let path = Path::new("C:\\Work\\locked-project");
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied.");
        let error = project_directory_access_error(path, &denied);

        assert!(matches!(
            error,
            AgentError::ProjectDirectoryNotAccessible { .. }
        ));
        let message = error.to_string();
        assert!(
            message.contains("C:\\Work\\locked-project"),
            "the message must name the folder: {message}"
        );
        assert!(
            message.contains("permission"),
            "the message must say what to check: {message}"
        );
        assert!(
            message.contains("Access is denied."),
            "the platform detail must be kept: {message}"
        );
        // The platform half is the only free text in the message, and it comes
        // from the file system - never from a session's environment.
        assert!(!message.contains("KEY_A"), "leaked: {message}");
    }

    /// A probe failure that is not a permission problem (a device error, a
    /// folder on a disconnected share) is reported the same specific way rather
    /// than being mislabelled as an invalid path.
    #[test]
    fn a_project_folder_that_fails_for_another_reason_is_reported_as_well() {
        let path = Path::new("Z:\\work\\disconnected");
        let device = std::io::Error::new(std::io::ErrorKind::NotConnected, "device not ready");
        let error = project_directory_access_error(path, &device);

        assert!(matches!(
            error,
            AgentError::ProjectDirectoryNotAccessible { .. }
        ));
        assert!(error.to_string().contains("device not ready"));
        assert!(!error.to_string().contains("KEY_A"));
    }
}
