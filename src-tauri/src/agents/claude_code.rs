//! Claude Code agent adapter (spec section 6).
//!
//! Responsibilities: executable discovery, per-session environment construction
//! (spec section 7), and the spawn description the PTY manager needs -
//! executable, base arguments, working directory (the project folder) and the
//! fully built environment (spec section 8). PTY wiring and process
//! supervision live in [`crate::pty`] and [`crate::sessions`]; nothing here
//! implements terminal emulation.

use std::env;
use std::path::{Path, PathBuf};

use crate::process::ProcessSpawn;
use crate::providers::ResolvedProvider;

use super::{AgentAdapter, AgentError, AgentState, SpawnRequest};

/// Stable adapter id persisted in `workspaces.agent_id`.
pub const AGENT_ID: &str = "claude-code";

/// Executable name used for PATH discovery. Claude Code ships as the `claude`
/// CLI (npm global installs, native installer, etc.), so it must be resolved
/// via PATH rather than a fixed location (spec section 6).
const CLAUDE_EXECUTABLE_NAME: &str = "claude";

/// Base URL of the Anthropic-compatible API to route requests through
/// ("Override the API endpoint to route requests through a proxy or gateway").
///
/// Source of truth (spec section 7): the official Claude Code environment
/// variable reference, <https://code.claude.com/docs/en/env-vars>, checked
/// 2026-09-14.
pub const ENV_BASE_URL: &str = "ANTHROPIC_BASE_URL";

/// Session credential. Documented as the "Custom value for the `Authorization`
/// header (prefixed with `Bearer `)"; when present it replaces subscription
/// authentication. This is the variable the workspace sets from the provider's
/// keyring secret - see [`claude_environment`] and the note on
/// [`ENV_API_KEY`] for why the bearer token is the one credential style a
/// session is given.
pub const ENV_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";

/// API key. Documented as "API key sent as `X-Api-Key` header".
///
/// Deliberately **not** emitted by [`claude_environment`]: the gateways this
/// application talks to authenticate the session credential as a bearer token
/// ([`ENV_AUTH_TOKEN`]), and presenting two credential styles at once is not a
/// documented configuration - a gateway that expects `Authorization: Bearer`
/// answers HTTP 401 to a request that also carries `X-Api-Key`, which is what
/// users reported. A gateway that really wants `X-Api-Key` still gets it
/// through the provider's *extra environment variables*, which are applied
/// last and therefore win (see [`claude_environment`]). The constant remains so
/// the absence is assertable by name rather than by string literal.
pub const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";

/// Model id ("Name of the model setting to use").
pub const ENV_MODEL: &str = "ANTHROPIC_MODEL";

/// The model's real context window, in tokens, for a model Claude Code's own
/// shipped catalog does not describe. A gateway can serve a model id the
/// catalog has never heard of - the user's `glm-5.3` is one - and Claude Code
/// then warns that the model is not described by this version's catalog and
/// sizes auto-compact from an assumed 200k window; this variable is what that
/// warning names as the way to declare the real window.
///
/// Source note: the variable is taken from Claude Code's own warning text as
/// the user reported it. It does **not** appear on the environment-variable
/// reference this project checks for the other names
/// (<https://code.claude.com/docs/en/env-vars>, fetched 2026-09-15), so unlike
/// [`ENV_AUTH_TOKEN`]/[`ENV_API_KEY`] its description here is not a verbatim
/// quote from that page.
///
/// Set from the provider profile's optional `max_context_tokens`, and applied
/// before the provider's extra environment variables so an extra can still
/// override it. Empty when the profile declares none, which is the right
/// default: a wrong window is worse than the documented fallback.
pub const ENV_MAX_CONTEXT_TOKENS: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";

/// Relocates Claude Code's configuration directory (settings, session history,
/// plugins). Reserved for per-session isolation in Milestone 3 (spec section 8);
/// documented at <https://code.claude.com/docs/en/settings>.
pub const ENV_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

pub struct ClaudeCodeAdapter {
    state: AgentState,
}

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self {
            state: AgentState::Created,
        }
    }

    /// Build the spawn description for an explicit executable.
    ///
    /// Split out of [`AgentAdapter::spawn_description`] so a test can substitute
    /// the program (and still exercise this production code path, including the
    /// environment and the config-directory isolation) on a machine where
    /// Claude Code is not installed (spec section 23).
    ///
    /// The environment is `build_environment` plus `CLAUDE_CONFIG_DIR` **last**,
    /// so a provider's extra variables cannot redirect the session away from its
    /// own configuration directory: config isolation (spec section 8) must not
    /// be optional.
    pub(crate) fn build_spawn(
        executable: &Path,
        args: &[String],
        request: &SpawnRequest,
    ) -> Result<ProcessSpawn, AgentError> {
        // The project folder is the agent's working directory, so a missing or
        // non-directory path must fail the start attempt with a clear message
        // (spec section 16: "Project directory deleted").
        if !request.project_path.exists() {
            return Err(AgentError::ProjectDirectoryMissing {
                path: request.project_path.display().to_string(),
            });
        }
        if !request.project_path.is_dir() {
            return Err(AgentError::ProjectDirectoryInvalid {
                path: request.project_path.display().to_string(),
            });
        }

        // ... and that it can actually be opened. `is_dir()` proves the path is
        // a directory, not that this process may use it; without this probe the
        // failure surfaces much later and much less clearly, from inside the
        // spawn (`CreateProcessW`'s "The directory name is invalid." or a bare
        // `EACCES`), which names neither the folder nor the fix (spec section
        // 16: "Permission errors").
        check_project_directory_access(&request.project_path)?;

        // Per-session configuration directory (settings, session history,
        // plugins). Created here because it is only needed by Claude Code.
        std::fs::create_dir_all(&request.config_dir).map_err(|error| {
            AgentError::ConfigDirectory {
                path: request.config_dir.display().to_string(),
                message: error.to_string(),
            }
        })?;

        let mut environment = claude_environment(&request.provider);
        environment.push((
            ENV_CONFIG_DIR.to_string(),
            request.config_dir.display().to_string(),
        ));

        Ok(ProcessSpawn::new(
            executable.to_path_buf(),
            args.to_vec(),
            request.project_path.clone(),
            environment,
        ))
    }
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// The documented Anthropic environment for one session (spec section 7).
///
/// Ordering: base URL, then the credential, then the model, then the declared
/// context window (when the profile sets one), then the provider's
/// user-configured extras. Extras come last and therefore win on duplicates,
/// which is what makes exotic gateway setups expressible - including
/// `ANTHROPIC_API_KEY` for a gateway that wants `X-Api-Key` instead of a bearer
/// token, or setting a variable to an empty string to neutralize it.
///
/// The credential is emitted as [`ENV_AUTH_TOKEN`] (`Authorization: Bearer`)
/// and **never** as [`ENV_API_KEY`]: the gateways configured here authenticate
/// the bearer token, and sending `X-Api-Key` alongside it draws HTTP 401
/// responses (spec section 5).
///
/// Can contain the API key: never log or serialize the result (spec section 17).
fn claude_environment(provider: &ResolvedProvider) -> Vec<(String, String)> {
    let mut environment: Vec<(String, String)> = Vec::new();

    if !provider.base_url.trim().is_empty() {
        environment.push((
            ENV_BASE_URL.to_string(),
            provider.base_url.trim().to_string(),
        ));
    }
    if let Some(auth_token) = provider.api_key() {
        if !auth_token.trim().is_empty() {
            environment.push((ENV_AUTH_TOKEN.to_string(), auth_token.trim().to_string()));
        }
    }
    if !provider.model.trim().is_empty() {
        environment.push((ENV_MODEL.to_string(), provider.model.trim().to_string()));
    }
    if let Some(max_context_tokens) = provider.max_context_tokens {
        environment.push((
            ENV_MAX_CONTEXT_TOKENS.to_string(),
            max_context_tokens.to_string(),
        ));
    }

    for (key, value) in &provider.extra_env {
        environment.push((key.clone(), value.clone()));
    }

    environment
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        AGENT_ID
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn is_installed(&self) -> bool {
        self.executable_path().is_some()
    }

    fn executable_path(&self) -> Option<PathBuf> {
        find_in_path(CLAUDE_EXECUTABLE_NAME)
    }

    /// Build this session's provider environment (spec sections 5, 7).
    ///
    /// The result is layered on top of the inherited parent environment when
    /// the process is spawned - it never modifies the user's global
    /// environment, and every session gets its own copy (spec section 7).
    /// Values can contain the API key, so the vector must not be logged or sent
    /// to the frontend.
    fn build_environment(&self, provider: &ResolvedProvider) -> Vec<(String, String)> {
        claude_environment(provider)
    }

    /// Claude Code is started as an interactive CLI: no base arguments
    /// (spec section 6 - "run the real Claude Code process through a PTY").
    fn base_arguments(&self) -> Vec<String> {
        Vec::new()
    }

    /// Resolve the full spawn description for one session (spec sections 6-8).
    fn spawn_description(&self, request: &SpawnRequest) -> Result<ProcessSpawn, AgentError> {
        let executable = self.executable_path().ok_or(AgentError::NotInstalled {
            agent: self.name(),
        })?;
        Self::build_spawn(&executable, &self.base_arguments(), request)
    }

    fn state(&self) -> AgentState {
        self.state
    }
}

/// Check that the project folder can be opened, so a permission problem is
/// reported as itself (spec section 16) instead of as a spawn failure.
///
/// Opening the directory for reading is the cheapest faithful probe: on both
/// platforms that is where the OS applies the access check, and a session
/// cannot run against a folder whose contents the agent may not read anyway
/// (Claude Code reads the project). Only the first entry (if any) is inspected;
/// an empty project folder is perfectly valid.
fn check_project_directory_access(path: &Path) -> Result<(), AgentError> {
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
fn project_directory_access_error(path: &Path, error: &std::io::Error) -> AgentError {
    AgentError::ProjectDirectoryNotAccessible {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

/// `which`-style PATH lookup, implemented with std only.
///
/// Platform notes (spec section 18): on Windows the CLI may be a `.exe`,
/// `.cmd`, or `.bat` shim (e.g. npm global installs), so the common PATHEXT
/// extensions are tried; on Unix the candidate must be a regular file with an
/// executable bit.
#[cfg(windows)]
fn find_in_path(executable: &str) -> Option<PathBuf> {
    find_in_path_with(&env::var_os("PATH")?, executable)
}

#[cfg(not(windows))]
fn find_in_path(executable: &str) -> Option<PathBuf> {
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
    use crate::providers::ProviderProfile;

    /// Build a resolved provider the way the command layer does: a stored
    /// profile plus the key read from the secret store.
    fn resolved(base_url: &str, model: &str, api_key: Option<&str>) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: base_url.to_string(),
                model: model.to_string(),
                extra_env: Vec::new(),
                max_context_tokens: None,
            },
            api_key.map(|key| key.to_string()),
        )
    }

    /// The same, with a declared context window - the `glm-5.3` case the user
    /// reported, where Claude Code's catalog does not describe the model.
    fn resolved_with_context_window(
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
        max_context_tokens: Option<u32>,
    ) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: base_url.to_string(),
                model: model.to_string(),
                extra_env: Vec::new(),
                max_context_tokens,
            },
            api_key.map(|key| key.to_string()),
        )
    }

    /// Last value for `key` - later entries override earlier ones, so this is
    /// what a spawned process would actually see.
    fn env_value(environment: &[(String, String)], key: &str) -> Option<String> {
        environment
            .iter()
            .filter(|(name, _value)| name == key)
            .next_back()
            .map(|(_name, value)| value.clone())
    }

    #[test]
    fn builds_the_documented_anthropic_environment() {
        let adapter = ClaudeCodeAdapter::new();
        let environment = adapter.build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            Some("KEY_A"),
        ));

        assert_eq!(
            env_value(&environment, ENV_BASE_URL),
            Some("https://provider-a.example.com".to_string())
        );
        // The credential goes out as a bearer token ...
        assert_eq!(
            env_value(&environment, ENV_AUTH_TOKEN),
            Some("KEY_A".to_string())
        );
        // ... and *not* as an API key header: a gateway that authenticates with
        // `Authorization: Bearer` answers HTTP 401 when `x-api-key` is also
        // presented, which is the failure the user reported.
        assert_eq!(
            env_value(&environment, ENV_API_KEY),
            None,
            "ANTHROPIC_API_KEY must never be emitted by default"
        );
        assert!(
            !environment
                .iter()
                .any(|(name, _value)| name == ENV_API_KEY),
            "the built environment must not contain {ENV_API_KEY}: {environment:?}"
        );
        assert_eq!(
            env_value(&environment, ENV_MODEL),
            Some("model-a".to_string())
        );

        // The default set is exactly these three variables, in this order;
        // ANTHROPIC_API_KEY and CLAUDE_CODE_MAX_CONTEXT_TOKENS are opt-in
        // (the former through extra env vars, the latter through the profile's
        // declared context window).
        assert_eq!(
            environment
                .iter()
                .map(|(name, _value)| name.as_str())
                .collect::<Vec<_>>(),
            vec![ENV_BASE_URL, ENV_AUTH_TOKEN, ENV_MODEL]
        );

        assert_eq!(adapter.id(), "claude-code");
        assert_eq!(adapter.name(), "Claude Code");
    }

    #[test]
    fn sessions_against_different_providers_get_different_environments() {
        // The core isolation requirement from spec section 7, at the
        // environment-construction level.
        let adapter = ClaudeCodeAdapter::new();

        let environment_a = adapter.build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            Some("KEY_A"),
        ));
        let environment_b = adapter.build_environment(&resolved(
            "https://provider-b.example.com",
            "model-b",
            Some("KEY_B"),
        ));

        assert_eq!(
            env_value(&environment_a, ENV_BASE_URL),
            Some("https://provider-a.example.com".to_string())
        );
        assert_eq!(
            env_value(&environment_b, ENV_BASE_URL),
            Some("https://provider-b.example.com".to_string())
        );
        assert_eq!(
            env_value(&environment_a, ENV_AUTH_TOKEN),
            Some("KEY_A".to_string())
        );
        assert_eq!(
            env_value(&environment_b, ENV_AUTH_TOKEN),
            Some("KEY_B".to_string())
        );
        // Neither session is given an API-key header (spec section 5).
        assert!(!environment_a.iter().any(|(name, _value)| name == ENV_API_KEY));
        assert!(!environment_b.iter().any(|(name, _value)| name == ENV_API_KEY));
        // No value from A leaked into B.
        assert!(!environment_b
            .iter()
            .any(|(_name, value)| value == "KEY_A" || value == "model-a"));
        assert_ne!(environment_a, environment_b);
    }

    #[test]
    fn extra_environment_variables_are_applied_last_and_win() {
        let adapter = ClaudeCodeAdapter::new();
        let provider = ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "model-a".to_string(),
                extra_env: vec![
                    ("ENABLE_TOOL_SEARCH".to_string(), "true".to_string()),
                    (ENV_API_KEY.to_string(), "KEY_A_FOR_X_API_KEY_GATEWAY".to_string()),
                    (ENV_MODEL.to_string(), "model-a-overridden".to_string()),
                ],
                max_context_tokens: None,
            },
            Some("KEY_A".to_string()),
        );

        let environment = adapter.build_environment(&provider);

        assert_eq!(
            env_value(&environment, "ENABLE_TOOL_SEARCH"),
            Some("true".to_string())
        );
        // The default credential (bearer) is still there, in the earlier slot.
        assert_eq!(
            env_value(&environment, ENV_AUTH_TOKEN),
            Some("KEY_A".to_string())
        );
        // An extra CAN deliberately add the API-key header style, and it wins:
        // the extra is the only source of ANTHROPIC_API_KEY, so a gateway that
        // wants x-api-key is still expressible.
        assert_eq!(
            env_value(&environment, ENV_API_KEY),
            Some("KEY_A_FOR_X_API_KEY_GATEWAY".to_string())
        );
        // Later entries override earlier ones; only one value would reach the
        // child process.
        assert_eq!(
            env_value(&environment, ENV_MODEL),
            Some("model-a-overridden".to_string())
        );
        assert_eq!(
            environment
                .iter()
                .filter(|(name, _value)| name == ENV_MODEL)
                .count(),
            2
        );
    }

    #[test]
    fn blank_fields_and_a_missing_key_are_omitted() {
        let adapter = ClaudeCodeAdapter::new();

        // No key stored: valid for endpoints that need none.
        let without_key = adapter.build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            None,
        ));
        assert_eq!(env_value(&without_key, ENV_AUTH_TOKEN), None);
        assert_eq!(env_value(&without_key, ENV_API_KEY), None);
        assert!(env_value(&without_key, ENV_BASE_URL).is_some());

        // Blank fields must not produce empty environment variables, which
        // Claude Code would otherwise treat as explicit overrides.
        let blank = adapter.build_environment(&resolved("   ", "  ", Some("  ")));
        assert!(blank.is_empty(), "unexpected environment: {blank:?}");
    }

    #[test]
    fn values_are_trimmed_for_display_consistency_with_validation() {
        let adapter = ClaudeCodeAdapter::new();
        let environment = adapter.build_environment(&resolved(
            " https://provider-a.example.com ",
            " model-a ",
            Some(" KEY_A "),
        ));

        assert_eq!(
            env_value(&environment, ENV_BASE_URL),
            Some("https://provider-a.example.com".to_string())
        );
        assert_eq!(
            env_value(&environment, ENV_MODEL),
            Some("model-a".to_string())
        );
        assert_eq!(
            env_value(&environment, ENV_AUTH_TOKEN),
            Some("KEY_A".to_string())
        );
    }

    /// The `glm-5.3` case (FIX 2): a declared context window reaches the session
    /// as `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, and nothing is emitted when the
    /// profile declares none.
    #[test]
    fn a_declared_context_window_is_exported_for_claude_code() {
        let adapter = ClaudeCodeAdapter::new();

        let declared = adapter.build_environment(&resolved_with_context_window(
            "https://provider-a.example.com",
            "glm-5.3",
            Some("KEY_A"),
            Some(1_000_000),
        ));
        assert_eq!(
            env_value(&declared, ENV_MAX_CONTEXT_TOKENS),
            Some("1000000".to_string())
        );
        // The declaration does not disturb the credential contract.
        assert_eq!(
            env_value(&declared, ENV_AUTH_TOKEN),
            Some("KEY_A".to_string())
        );
        assert!(!declared.iter().any(|(name, _value)| name == ENV_API_KEY));

        // No declaration: Claude Code's own fallback stays in charge, which
        // means no variable at all rather than an empty one.
        let undeclared = adapter.build_environment(&resolved_with_context_window(
            "https://provider-a.example.com",
            "glm-5.3",
            Some("KEY_A"),
            None,
        ));
        assert_eq!(env_value(&undeclared, ENV_MAX_CONTEXT_TOKENS), None);
    }

    /// The declared window sits *before* the extras, so a provider that needs a
    /// different value there can still say so (the same rule the other default
    /// variables follow).
    #[test]
    fn the_declared_context_window_can_be_overridden_by_an_extra() {
        let adapter = ClaudeCodeAdapter::new();
        let provider = ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "glm-5.3".to_string(),
                extra_env: vec![(
                    ENV_MAX_CONTEXT_TOKENS.to_string(),
                    "131072".to_string(),
                )],
                max_context_tokens: Some(1_000_000),
            },
            Some("KEY_A".to_string()),
        );

        let environment = adapter.build_environment(&provider);

        assert_eq!(
            env_value(&environment, ENV_MAX_CONTEXT_TOKENS),
            Some("131072".to_string()),
            "the extra must win"
        );
        assert_eq!(
            environment
                .iter()
                .filter(|(name, _value)| name == ENV_MAX_CONTEXT_TOKENS)
                .count(),
            2,
            "the default is still emitted before the extra"
        );
    }

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

    // --- spawn description (Milestone 3) ------------------------------------

    #[cfg(test)]
    mod spawn {
        use super::*;
        use crate::agents::{AgentAdapter, AgentError, SpawnRequest};
        use crate::persistence::test_support::TempDir;
        use std::ffi::OsString;

        fn request(project: &Path, config: &Path, provider: ResolvedProvider) -> SpawnRequest {
            SpawnRequest {
                project_path: project.to_path_buf(),
                config_dir: config.to_path_buf(),
                provider,
            }
        }

        fn resolved_with_extras(extra_env: Vec<(String, String)>) -> ResolvedProvider {
            ResolvedProvider::new(
                ProviderProfile {
                    id: "provider-a-id".to_string(),
                    name: "Provider A".to_string(),
                    base_url: "https://provider-a.example.com".to_string(),
                    model: "model-a".to_string(),
                    extra_env,
                    max_context_tokens: None,
                },
                Some("KEY_A".to_string()),
            )
        }

        #[test]
        fn spawn_description_targets_the_project_folder_with_an_isolated_config_dir() {
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let config_dir = config_root.join("session-1");

            let spawn = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(project.path(), &config_dir, resolved_with_extras(Vec::new())),
            )
            .expect("build a spawn description");

            assert_eq!(spawn.program, PathBuf::from("claude"));
            // An interactive session: no base arguments (spec section 6).
            assert!(spawn.args.is_empty());
            // Working directory is the workspace's project folder (spec section 8).
            assert_eq!(spawn.cwd, project.path());
            // Provider environment plus per-session configuration isolation.
            assert_eq!(
                spawn.environment_value(ENV_BASE_URL),
                Some("https://provider-a.example.com")
            );
            assert_eq!(
                spawn.environment_value(ENV_AUTH_TOKEN),
                Some("KEY_A"),
                "a session authenticates with the bearer token"
            );
            assert_eq!(
                spawn.environment_value(ENV_API_KEY),
                None,
                "a session must not also send x-api-key"
            );
            assert_eq!(spawn.environment_value(ENV_MODEL), Some("model-a"));
            assert_eq!(
                spawn.environment_value(ENV_CONFIG_DIR),
                Some(config_dir.display().to_string().as_str())
            );
            // The configuration directory was created for the session.
            assert!(config_dir.is_dir(), "the session config dir must exist");

            // The spawn description must not print the credential (spec section 17).
            let rendered = format!("{spawn:?}");
            assert!(!rendered.contains("KEY_A"), "leaked: {rendered}");
        }

        /// The exact variable-name list one session is given, in order - the
        /// wire contract of FIX 1 and FIX 2 in one place: the credential is
        /// `ANTHROPIC_AUTH_TOKEN` (never `ANTHROPIC_API_KEY`), the declared
        /// context window rides along when the profile sets one, the provider's
        /// extras follow, and the config directory is last.
        #[test]
        fn the_session_environment_names_are_exactly_the_documented_set() {
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let provider = ResolvedProvider::new(
                ProviderProfile {
                    id: "provider-a-id".to_string(),
                    name: "Provider A".to_string(),
                    base_url: "https://provider-a.example.com".to_string(),
                    model: "glm-5.3".to_string(),
                    extra_env: vec![("ENABLE_TOOL_SEARCH".to_string(), "true".to_string())],
                    max_context_tokens: Some(1_000_000),
                },
                Some("KEY_A".to_string()),
            );

            let spawn = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(
                    project.path(),
                    &config_root.join("session-1"),
                    provider,
                ),
            )
            .expect("build a spawn description");

            let names: Vec<&str> = spawn
                .environment
                .iter()
                .map(|(name, _value)| name.as_str())
                .collect();
            assert_eq!(
                names,
                vec![
                    ENV_BASE_URL,
                    ENV_AUTH_TOKEN,
                    ENV_MODEL,
                    ENV_MAX_CONTEXT_TOKENS,
                    "ENABLE_TOOL_SEARCH",
                    ENV_CONFIG_DIR,
                ]
            );
            // Printed so an evidence run can quote the list verbatim:
            // `cargo test --lib -- --nocapture the_session_environment_names`.
            println!("session environment variables: {}", names.join(", "));
        }

        #[test]
        fn two_sessions_get_different_configuration_directories() {
            // Spec section 8: session A != session B, even for the same provider.
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let provider = resolved_with_extras(Vec::new());

            let spawn_a = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(project.path(), &config_root.join("session-a"), provider.clone()),
            )
            .unwrap();
            let spawn_b = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(project.path(), &config_root.join("session-b"), provider),
            )
            .unwrap();

            assert_ne!(
                spawn_a.environment_value(ENV_CONFIG_DIR),
                spawn_b.environment_value(ENV_CONFIG_DIR)
            );
        }

        #[test]
        fn provider_extras_cannot_redirect_the_session_config_directory() {
            // Config isolation is not optional: CLAUDE_CONFIG_DIR is applied
            // last, so a provider profile cannot point two sessions at the same
            // configuration directory.
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let config_dir = config_root.join("session-1");

            let spawn = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(
                    project.path(),
                    &config_dir,
                    resolved_with_extras(vec![(
                        ENV_CONFIG_DIR.to_string(),
                        "C:\\shared\\claude-config".to_string(),
                    )]),
                ),
            )
            .unwrap();

            assert_eq!(
                spawn.environment_value(ENV_CONFIG_DIR),
                Some(config_dir.display().to_string().as_str())
            );
        }

        #[test]
        fn a_deleted_project_folder_fails_the_spawn_description() {
            let config_root = TempDir::new("agent-config");
            let missing = config_root.join("does-not-exist");

            let error = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(&missing, &config_root.join("session-1"), resolved_with_extras(Vec::new())),
            )
            .expect_err("a missing project folder must fail");
            assert!(matches!(error, AgentError::ProjectDirectoryMissing { .. }));
            assert!(error.to_string().contains("does-not-exist"));
        }

        #[test]
        fn a_project_path_that_is_a_file_fails_the_spawn_description() {
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let file = project.join("a-file.txt");
            std::fs::write(&file, b"not a folder").unwrap();

            let error = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(&file, &config_root.join("session-1"), resolved_with_extras(Vec::new())),
            )
            .expect_err("a file is not a project folder");
            assert!(matches!(error, AgentError::ProjectDirectoryInvalid { .. }));
        }

        #[test]
        fn a_config_directory_that_cannot_be_created_fails_the_spawn_description() {
            let project = TempDir::new("agent-project");
            let blocker = TempDir::new("agent-config-blocker");
            // A *file* where the config directory's parent needs to be.
            let blocked_parent = blocker.join("blocked");
            std::fs::write(&blocked_parent, b"not a folder").unwrap();

            let error = ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(
                    project.path(),
                    &blocked_parent.join("session-1"),
                    resolved_with_extras(Vec::new()),
                ),
            )
            .expect_err("an unusable config directory must fail");
            assert!(matches!(error, AgentError::ConfigDirectory { .. }));
            // No credential may appear in the message (spec section 17).
            assert!(!error.to_string().contains("KEY_A"));
        }

        /// The access probe (spec section 16: "Permission errors", "Project
        /// directory deleted").
        ///
        /// A readable folder - including an empty one - must never be refused:
        /// the probe exists to turn an OS-level permission failure into a clear
        /// message, not to add a new way for a valid project to be rejected.
        #[test]
        fn a_readable_project_folder_passes_the_access_probe() {
            let empty = TempDir::new("agent-probe-empty");
            assert!(
                check_project_directory_access(empty.path()).is_ok(),
                "an empty project folder is valid"
            );

            let populated = TempDir::new("agent-probe-populated");
            std::fs::write(populated.join("main.rs"), b"fn main() {}").unwrap();
            std::fs::create_dir_all(populated.join("src")).unwrap();
            assert!(check_project_directory_access(populated.path()).is_ok());
        }

        /// The message a permission failure produces.
        ///
        /// The failing `read_dir` is the *same* call the probe makes, with the
        /// error kind the operating system raises for an unreadable directory
        /// (`EACCES`/`ERROR_ACCESS_DENIED` both map to `PermissionDenied`), so
        /// what is asserted here is the production mapping the user reads.
        #[test]
        fn a_project_folder_that_cannot_be_opened_reports_an_actionable_message() {
            let path = Path::new("C:\\Work\\locked-project");
            let denied =
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied.");
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
            // The platform half is the only free text in the message, and it
            // comes from the file system - never from a session's environment.
            assert!(!message.contains("KEY_A"), "leaked: {message}");
        }

        /// A probe failure that is not a permission problem (a device error, a
        /// folder on a disconnected share) is reported the same specific way
        /// rather than being mislabelled as an invalid path.
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

        /// The probe is wired into the spawn description (positive control), so
        /// a folder it refuses never reaches the PTY: the user's message stays
        /// about the folder instead of about a failed process creation.
        #[test]
        fn the_spawn_description_calls_the_access_probe_before_returning() {
            let project = TempDir::new("agent-probe-spawn");
            let config_root = TempDir::new("agent-config");
            assert!(check_project_directory_access(project.path()).is_ok());
            assert!(ClaudeCodeAdapter::build_spawn(
                Path::new("claude"),
                &[],
                &request(
                    project.path(),
                    &config_root.join("session-1"),
                    resolved_with_extras(Vec::new()),
                ),
            )
            .is_ok());
        }

        #[test]
        fn a_fake_program_keeps_the_production_environment_path() {
            // The seam the PTY/session tests rely on: substituting the program
            // changes nothing else about the spawn description.
            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");

            let spawn = ClaudeCodeAdapter::build_spawn(
                Path::new("cmd.exe"),
                &["/k".to_string()],
                &request(
                    project.path(),
                    &config_root.join("session-1"),
                    resolved_with_extras(Vec::new()),
                ),
            )
            .unwrap();

            assert_eq!(spawn.program, PathBuf::from("cmd.exe"));
            assert_eq!(spawn.args, vec!["/k".to_string()]);
            assert_eq!(
                spawn.environment_value(ENV_BASE_URL),
                Some("https://provider-a.example.com")
            );
        }

        #[test]
        fn executable_discovery_searches_the_given_path_value() {
            // Executable discovery without touching the test process's PATH.
            let directory = TempDir::new("agent-path");
            let name = if cfg!(windows) {
                "claude.cmd"
            } else {
                "claude"
            };
            let candidate = directory.join(name);
            std::fs::write(&candidate, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }

            let path_var = OsString::from(directory.path().as_os_str());
            assert_eq!(find_in_path_with(&path_var, "claude"), Some(candidate));
            assert_eq!(find_in_path_with(&path_var, "definitely-not-a-real-cli"), None);

            // A non-executable file on Unix must not be treated as the CLI.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o644))
                    .unwrap();
                assert_eq!(find_in_path_with(&path_var, "claude"), None);
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

        #[test]
        fn the_real_adapter_reports_its_own_installation_state() {
            // Machine-dependent by design, so the assertions are conditional:
            // this test never requires Claude Code to be installed, but when it
            // is (a developer machine), it exercises the real discovery path -
            // including Windows' `.cmd` shim for npm global installs.
            let adapter = ClaudeCodeAdapter::new();
            assert_eq!(adapter.is_installed(), adapter.executable_path().is_some());

            let project = TempDir::new("agent-project");
            let config_root = TempDir::new("agent-config");
            let request = request(
                project.path(),
                &config_root.join("session-1"),
                resolved_with_extras(Vec::new()),
            );

            match adapter.executable_path() {
                Some(executable) => {
                    println!("discovered Claude Code at {}", executable.display());
                    let spawn = adapter
                        .spawn_description(&request)
                        .expect("a discovered executable must produce a spawn description");
                    // Discovery and the spawn description must agree.
                    assert_eq!(spawn.program, executable);
                    assert!(spawn.args.is_empty(), "an interactive session has no base arguments");
                    assert_eq!(spawn.cwd, project.path());
                }
                None => {
                    println!("Claude Code is not installed on this machine");
                    let error = adapter
                        .spawn_description(&request)
                        .expect_err("no CLI means no spawn description");
                    assert!(matches!(error, AgentError::NotInstalled { .. }));
                    assert!(error.to_string().contains("Claude Code"));
                }
            }
        }
    }
}
