//! OpenAI Codex CLI agent adapter (spec section 6).
//!
//! The second adapter behind [`AgentAdapter`], and the reason the trait exists:
//! a Codex session reuses the same discovery, spawn-description, PTY and
//! session-lifecycle code as a Claude Code session, and differs only in the two
//! agent-specific answers - the environment and the arguments.
//!
//! | | Claude Code | Codex CLI |
//! | --- | --- | --- |
//! | executable | `claude` | `codex` |
//! | endpoint | `ANTHROPIC_BASE_URL` | `OPENAI_BASE_URL` |
//! | credential | `ANTHROPIC_AUTH_TOKEN` | `OPENAI_API_KEY` |
//! | model | `ANTHROPIC_MODEL` | `--model` argument |
//! | config directory | `CLAUDE_CONFIG_DIR` | `CODEX_HOME` |
//!
//! The shared half of a spawn description - working directory, project-folder
//! validation, the per-session configuration directory - lives in
//! [`super::spawn`], so adding a third agent means writing this file and a
//! registry entry, not touching [`crate::sessions`].

use std::path::{Path, PathBuf};

use crate::process::ProcessSpawn;
use crate::providers::ResolvedProvider;

use super::executable::find_in_path;
use super::spawn::build_session_spawn;
use super::{AgentAdapter, AgentError, AgentState, SpawnRequest};

/// Stable adapter id persisted in `workspaces.agent_id`.
pub const AGENT_ID: &str = "codex";

/// Executable name used for PATH discovery. The Codex CLI ships as `codex`
/// (npm global install `@openai/codex`, Homebrew, or a release binary), so it is
/// resolved through `PATH` rather than a fixed location (spec section 6).
const CODEX_EXECUTABLE_NAME: &str = "codex";

/// Relocates Codex's configuration directory, which holds `config.toml`, the
/// stored credential and the session logs. Defaults to `~/.codex`; overriding
/// it is what gives each session its own configuration (spec section 8), the
/// same role `CLAUDE_CONFIG_DIR` plays for Claude Code.
pub const ENV_HOME: &str = "CODEX_HOME";

/// The session credential, sent as `Authorization: Bearer`.
///
/// Unlike Claude Code - where the bearer token is deliberate and the API-key
/// header is never emitted - `OPENAI_API_KEY` *is* the documented Codex
/// credential, so this is the variable the workspace fills from the provider's
/// keyring secret (spec sections 5, 7).
pub const ENV_API_KEY: &str = "OPENAI_API_KEY";

/// Base URL of the OpenAI-compatible API to route requests through, so a
/// session can talk to a gateway or proxy instead of the public endpoint.
/// Overrides the built-in `openai` provider's base URL; a profile that needs a
/// custom provider block can still reach one through its extra environment
/// variables.
pub const ENV_BASE_URL: &str = "OPENAI_BASE_URL";

/// The model flag.
///
/// Codex has no documented model *environment variable*: the model is a
/// configuration value, settable from `config.toml` or from this flag
/// (precedence: command-line flag > profile > `config.toml` > built-in
/// default). Because the workspace's model is per-session state rather than
/// per-configuration-directory state, it travels as this argument - see
/// [`CodexAdapter::model_arguments`].
pub const ARG_MODEL: &str = "--model";

pub struct CodexAdapter {
    state: AgentState,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            state: AgentState::Created,
        }
    }

    /// The model flag for one session, empty when no model is configured.
    ///
    /// A gateway can serve a model id Codex has never heard of, and the model is
    /// a per-session choice: two workspaces may point at different providers
    /// with different models while sharing the machine. It therefore cannot live
    /// in Codex's configuration directory - which the session owns and which is
    /// created empty for exactly that reason - so it is passed as the documented
    /// [`ARG_MODEL`] flag instead.
    pub(crate) fn model_arguments(provider: &ResolvedProvider) -> Vec<String> {
        let model = provider.model.trim();
        if model.is_empty() {
            // No configured model: Codex's own default stays in charge, which is
            // the right fallback - a blank `--model` would be an explicit
            // request for a model named "".
            Vec::new()
        } else {
            vec![ARG_MODEL.to_string(), model.to_string()]
        }
    }

    /// The complete argument list for one session: this adapter's base
    /// arguments followed by the model flag.
    ///
    /// Split out from [`AgentAdapter::spawn_description`] so the arguments a real
    /// session is started with are assertable without an installed Codex.
    pub(crate) fn session_arguments(provider: &ResolvedProvider) -> Vec<String> {
        let mut args = CodexAdapter::new().base_arguments();
        args.extend(Self::model_arguments(provider));
        args
    }

    /// Build the spawn description for an explicit executable and argument list.
    ///
    /// Split out of [`AgentAdapter::spawn_description`] so a test can substitute
    /// the program (and still exercise this production code path, including the
    /// environment and the config-directory isolation) on a machine where Codex
    /// is not installed (spec section 23).
    ///
    /// The environment is `build_environment` plus `CODEX_HOME` **last**, so a
    /// provider's extra variables cannot redirect the session away from its own
    /// configuration directory: config isolation (spec section 8) must not be
    /// optional.
    pub(crate) fn build_spawn(
        executable: &Path,
        args: &[String],
        request: &SpawnRequest,
    ) -> Result<ProcessSpawn, AgentError> {
        build_session_spawn(
            executable,
            args,
            request,
            codex_environment(&request.provider),
            ENV_HOME,
        )
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// The documented OpenAI environment for one session (spec section 7).
///
/// Ordering: base URL, then the credential, then the provider's user-configured
/// extras. Extras come last and therefore win on duplicates, which is what makes
/// exotic gateway setups expressible - pointing a session at a different
/// endpoint, declaring a provider-specific key, or clearing a variable by
/// setting it to an empty string.
///
/// The model is deliberately **not** here: see [`ARG_MODEL`] and
/// [`CodexAdapter::model_arguments`]. A provider's extra variables may still
/// declare one for a Codex version that reads it, in which case the flag and the
/// variable are both present and the flag (command-line precedence) is what
/// Codex honours.
///
/// Can contain the API key: never log or serialize the result (spec section 17).
fn codex_environment(provider: &ResolvedProvider) -> Vec<(String, String)> {
    let mut environment: Vec<(String, String)> = Vec::new();

    if !provider.base_url.trim().is_empty() {
        environment.push((
            ENV_BASE_URL.to_string(),
            provider.base_url.trim().to_string(),
        ));
    }
    // The Codex credential is the API key itself (bearer), so unlike Claude Code
    // there is no alternative header style to prefer.
    if let Some(api_key) = provider.api_key() {
        if !api_key.trim().is_empty() {
            environment.push((ENV_API_KEY.to_string(), api_key.trim().to_string()));
        }
    }

    for (key, value) in &provider.extra_env {
        environment.push((key.clone(), value.clone()));
    }

    environment
}

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        AGENT_ID
    }

    fn name(&self) -> &'static str {
        "Codex"
    }

    fn is_installed(&self) -> bool {
        self.executable_path().is_some()
    }

    fn executable_path(&self) -> Option<PathBuf> {
        find_in_path(CODEX_EXECUTABLE_NAME)
    }

    /// Build this session's provider environment (spec sections 5, 7).
    ///
    /// The result is layered on top of the inherited parent environment when the
    /// process is spawned - it never modifies the user's global environment, and
    /// every session gets its own copy (spec section 7). Values can contain the
    /// API key, so the vector must not be logged or sent to the frontend.
    fn build_environment(&self, provider: &ResolvedProvider) -> Vec<(String, String)> {
        codex_environment(provider)
    }

    /// Codex is started as an interactive CLI: its own base arguments are empty,
    /// and the per-session model is appended by [`CodexAdapter::session_arguments`]
    /// (spec section 6 - "run the real Codex process through a PTY").
    fn base_arguments(&self) -> Vec<String> {
        Vec::new()
    }

    /// Resolve the full spawn description for one session (spec sections 6-8).
    fn spawn_description(&self, request: &SpawnRequest) -> Result<ProcessSpawn, AgentError> {
        let executable = self.executable_path().ok_or(AgentError::NotInstalled {
            agent: self.name(),
        })?;
        let args = Self::session_arguments(&request.provider);
        Self::build_spawn(&executable, &args, request)
    }

    fn state(&self) -> AgentState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::test_support::TempDir;
    use crate::providers::ProviderProfile;

    /// Build a resolved provider the way the command layer does: a stored
    /// profile plus the key read from the secret store.
    fn resolved(base_url: &str, model: &str, api_key: Option<&str>) -> ResolvedProvider {
        resolved_with_extras(base_url, model, api_key, Vec::new())
    }

    fn resolved_with_extras(
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
        extra_env: Vec<(String, String)>,
    ) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: base_url.to_string(),
                model: model.to_string(),
                extra_env,
                max_context_tokens: None,
            },
            api_key.map(|key| key.to_string()),
        )
    }

    /// Last value for `key` - later entries override earlier ones, so this is
    /// what a spawned process would actually see.
    fn env_value(environment: &[(String, String)], key: &str) -> Option<String> {
        environment
            .iter()
            .rfind(|(name, _value)| name == key)
            .map(|(_name, value)| value.clone())
    }

    #[test]
    fn builds_the_documented_openai_environment() {
        let adapter = CodexAdapter::new();
        let environment = adapter.build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            Some("KEY_A"),
        ));

        assert_eq!(
            env_value(&environment, ENV_BASE_URL),
            Some("https://provider-a.example.com".to_string())
        );
        assert_eq!(
            env_value(&environment, ENV_API_KEY),
            Some("KEY_A".to_string()),
            "OPENAI_API_KEY is the documented Codex credential"
        );

        // The default set is exactly these two variables, in this order. The
        // model is not among them: Codex has no documented model variable.
        assert_eq!(
            environment
                .iter()
                .map(|(name, _value)| name.as_str())
                .collect::<Vec<_>>(),
            vec![ENV_BASE_URL, ENV_API_KEY]
        );

        assert_eq!(adapter.id(), "codex");
        assert_eq!(adapter.name(), "Codex");
    }

    /// A Codex session must not be handed Claude Code's variables: a machine
    /// running both agents would otherwise let one session's contract leak into
    /// the other (spec section 7 - "the session environment is exactly what the
    /// agent documents").
    #[test]
    fn a_codex_session_gets_no_anthropic_variables() {
        let environment = CodexAdapter::new().build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            Some("KEY_A"),
        ));

        for name in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_MODEL",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        ] {
            assert!(
                !environment.iter().any(|(key, _value)| key == name),
                "{name} must not reach a Codex session: {environment:?}"
            );
        }
        // ... and neither do the Codex variables reach a Claude Code session.
        let claude = crate::agents::claude_code::ClaudeCodeAdapter::new()
            .build_environment(&resolved(
                "https://provider-a.example.com",
                "model-a",
                Some("KEY_A"),
            ));
        for name in [ENV_BASE_URL, ENV_API_KEY, ENV_HOME] {
            assert!(
                !claude.iter().any(|(key, _value)| key == name),
                "{name} must not reach a Claude Code session: {claude:?}"
            );
        }
    }

    /// The model is a command-line flag, not an environment variable.
    #[test]
    fn the_model_travels_as_the_documented_flag() {
        let provider = resolved("https://provider-a.example.com", "gpt-5-codex", Some("KEY_A"));

        assert_eq!(
            CodexAdapter::model_arguments(&provider),
            vec![ARG_MODEL.to_string(), "gpt-5-codex".to_string()]
        );
        // And a real session is started with exactly that.
        assert_eq!(
            CodexAdapter::session_arguments(&provider),
            vec![ARG_MODEL.to_string(), "gpt-5-codex".to_string()]
        );
        // The model must not also be exported as a variable: two sources of
        // truth for one setting is how a session ends up on the wrong model.
        let environment = CodexAdapter::new().build_environment(&provider);
        assert!(
            !environment
                .iter()
                .any(|(_name, value)| value == "gpt-5-codex"),
            "the model must not appear in the environment: {environment:?}"
        );
    }

    #[test]
    fn a_blank_model_produces_no_flag_and_is_trimmed_when_present() {
        // No configured model: Codex's own default stays in charge, which means
        // no flag at all rather than an empty one.
        assert!(CodexAdapter::model_arguments(&resolved(
            "https://provider-a.example.com",
            "   ",
            Some("KEY_A")
        ))
        .is_empty());
        assert!(CodexAdapter::session_arguments(&resolved(
            "https://provider-a.example.com",
            "",
            Some("KEY_A")
        ))
        .is_empty());

        assert_eq!(
            CodexAdapter::model_arguments(&resolved(
                "https://provider-a.example.com",
                " gpt-5-codex ",
                Some("KEY_A")
            )),
            vec![ARG_MODEL.to_string(), "gpt-5-codex".to_string()]
        );
    }

    #[test]
    fn sessions_against_different_providers_get_different_environments() {
        // The core isolation requirement from spec section 7, at the
        // environment-construction level.
        let adapter = CodexAdapter::new();

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
            env_value(&environment_a, ENV_API_KEY),
            Some("KEY_A".to_string())
        );
        assert_eq!(
            env_value(&environment_b, ENV_API_KEY),
            Some("KEY_B".to_string())
        );
        // No value from A leaked into B.
        assert!(!environment_b
            .iter()
            .any(|(_name, value)| value == "KEY_A" || value == "model-a"));
        assert_ne!(environment_a, environment_b);
    }

    #[test]
    fn extra_environment_variables_are_applied_last_and_win() {
        let adapter = CodexAdapter::new();
        let provider = resolved_with_extras(
            "https://provider-a.example.com",
            "model-a",
            Some("KEY_A"),
            vec![
                ("CODEX_DISABLE_TELEMETRY".to_string(), "1".to_string()),
                (ENV_BASE_URL.to_string(), "https://gateway.example.com/v1".to_string()),
                (ENV_API_KEY.to_string(), "KEY_A_FOR_GATEWAY".to_string()),
            ],
        );

        let environment = adapter.build_environment(&provider);

        assert_eq!(
            env_value(&environment, "CODEX_DISABLE_TELEMETRY"),
            Some("1".to_string())
        );
        // Later entries override earlier ones; only one value would reach the
        // child process.
        assert_eq!(
            env_value(&environment, ENV_BASE_URL),
            Some("https://gateway.example.com/v1".to_string())
        );
        assert_eq!(
            env_value(&environment, ENV_API_KEY),
            Some("KEY_A_FOR_GATEWAY".to_string())
        );
    }

    #[test]
    fn blank_fields_and_a_missing_key_are_omitted() {
        let adapter = CodexAdapter::new();

        // No key stored: valid for endpoints that need none (a local gateway).
        let without_key = adapter.build_environment(&resolved(
            "https://provider-a.example.com",
            "model-a",
            None,
        ));
        assert_eq!(env_value(&without_key, ENV_API_KEY), None);
        assert!(env_value(&without_key, ENV_BASE_URL).is_some());

        // Blank fields must not produce empty environment variables, which Codex
        // would otherwise treat as explicit overrides.
        let blank = adapter.build_environment(&resolved("   ", "  ", Some("  ")));
        assert!(blank.is_empty(), "unexpected environment: {blank:?}");
    }

    #[test]
    fn values_are_trimmed_for_display_consistency_with_validation() {
        let adapter = CodexAdapter::new();
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
            env_value(&environment, ENV_API_KEY),
            Some("KEY_A".to_string())
        );
    }

    /// A declared context window is a Claude Code concept: the profile field
    /// feeds `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, which no Codex version reads.
    ///
    /// Rather than guess at a Codex configuration key for it (a wrong key is
    /// rejected by Codex's own config validation, which would break the session
    /// outright), the field is deliberately not forwarded. The assertion pins
    /// that choice, so a future mapping is a deliberate change and not an
    /// accident.
    #[test]
    fn a_declared_context_window_is_not_forwarded_to_codex() {
        let provider = ResolvedProvider::new(
            ProviderProfile {
                id: "provider-a-id".to_string(),
                name: "Provider A".to_string(),
                base_url: "https://provider-a.example.com".to_string(),
                model: "gpt-5-codex".to_string(),
                extra_env: Vec::new(),
                max_context_tokens: Some(1_000_000),
            },
            Some("KEY_A".to_string()),
        );

        let environment = CodexAdapter::new().build_environment(&provider);
        assert_eq!(
            env_value(&environment, "CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            None
        );
        assert_eq!(
            environment
                .iter()
                .map(|(name, _value)| name.as_str())
                .collect::<Vec<_>>(),
            vec![ENV_BASE_URL, ENV_API_KEY],
            "the declared window adds nothing to a Codex session"
        );
        // A provider that really needs a Codex context-window setting can still
        // express it through its extra environment variables, which win.
    }

    // --- spawn description (Milestone 3) ------------------------------------

    fn request(project: &Path, config: &Path, provider: ResolvedProvider) -> SpawnRequest {
        SpawnRequest {
            project_path: project.to_path_buf(),
            config_dir: config.to_path_buf(),
            provider,
        }
    }

    #[test]
    fn spawn_description_targets_the_project_folder_with_an_isolated_codex_home() {
        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");
        let config_dir = config_root.join("session-1");
        let provider = resolved("https://provider-a.example.com", "gpt-5-codex", Some("KEY_A"));

        let spawn = CodexAdapter::build_spawn(
            Path::new("codex"),
            &CodexAdapter::session_arguments(&provider),
            &request(project.path(), &config_dir, provider),
        )
        .expect("build a spawn description");

        assert_eq!(spawn.program, PathBuf::from("codex"));
        // The model is the session's only argument (spec section 6).
        assert_eq!(spawn.args, vec![ARG_MODEL.to_string(), "gpt-5-codex".to_string()]);
        // Working directory is the workspace's project folder (spec section 8).
        assert_eq!(spawn.cwd, project.path());
        // Provider environment plus per-session configuration isolation.
        assert_eq!(
            spawn.environment_value(ENV_BASE_URL),
            Some("https://provider-a.example.com")
        );
        assert_eq!(spawn.environment_value(ENV_API_KEY), Some("KEY_A"));
        assert_eq!(
            spawn.environment_value(ENV_HOME),
            Some(config_dir.display().to_string().as_str())
        );
        // The configuration directory was created for the session.
        assert!(config_dir.is_dir(), "the session config dir must exist");
        // ... and no Claude Code variable came along.
        assert_eq!(spawn.environment_value("ANTHROPIC_BASE_URL"), None);
        assert_eq!(spawn.environment_value("CLAUDE_CONFIG_DIR"), None);

        // The spawn description must not print the credential (spec section 17).
        let rendered = format!("{spawn:?}");
        assert!(!rendered.contains("KEY_A"), "leaked: {rendered}");
    }

    /// The exact variable-name list one Codex session is given, in order: the
    /// endpoint, the credential, the provider's extras, and the config
    /// directory last.
    #[test]
    fn the_session_environment_names_are_exactly_the_documented_set() {
        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");
        let provider = resolved_with_extras(
            "https://provider-a.example.com",
            "gpt-5-codex",
            Some("KEY_A"),
            vec![("CODEX_DISABLE_TELEMETRY".to_string(), "1".to_string())],
        );

        let spawn = CodexAdapter::build_spawn(
            Path::new("codex"),
            &CodexAdapter::session_arguments(&provider),
            &request(project.path(), &config_root.join("session-1"), provider),
        )
        .expect("build a spawn description");

        let names: Vec<&str> = spawn
            .environment
            .iter()
            .map(|(name, _value)| name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![ENV_BASE_URL, ENV_API_KEY, "CODEX_DISABLE_TELEMETRY", ENV_HOME]
        );
        // Printed so an evidence run can quote the list verbatim:
        // `cargo test --lib -- --nocapture the_session_environment_names`.
        println!("codex session environment variables: {}", names.join(", "));
    }

    #[test]
    fn two_sessions_get_different_configuration_directories() {
        // Spec section 8: session A != session B, even for the same provider.
        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");
        let provider = resolved("https://provider-a.example.com", "model-a", Some("KEY_A"));

        let spawn_a = CodexAdapter::build_spawn(
            Path::new("codex"),
            &[],
            &request(project.path(), &config_root.join("session-a"), provider.clone()),
        )
        .unwrap();
        let spawn_b = CodexAdapter::build_spawn(
            Path::new("codex"),
            &[],
            &request(project.path(), &config_root.join("session-b"), provider),
        )
        .unwrap();

        assert_ne!(
            spawn_a.environment_value(ENV_HOME),
            spawn_b.environment_value(ENV_HOME)
        );
    }

    #[test]
    fn provider_extras_cannot_redirect_the_session_config_directory() {
        // Config isolation is not optional: CODEX_HOME is applied last, so a
        // provider profile cannot point two sessions at the same Codex
        // configuration directory (and therefore at the same stored credential).
        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");
        let config_dir = config_root.join("session-1");

        let spawn = CodexAdapter::build_spawn(
            Path::new("codex"),
            &[],
            &request(
                project.path(),
                &config_dir,
                resolved_with_extras(
                    "https://provider-a.example.com",
                    "model-a",
                    Some("KEY_A"),
                    vec![(ENV_HOME.to_string(), "C:\\shared\\codex-home".to_string())],
                ),
            ),
        )
        .unwrap();

        assert_eq!(
            spawn.environment_value(ENV_HOME),
            Some(config_dir.display().to_string().as_str())
        );
    }

    #[test]
    fn a_deleted_project_folder_fails_the_spawn_description() {
        let config_root = TempDir::new("codex-config");
        let missing = config_root.join("does-not-exist");

        let error = CodexAdapter::build_spawn(
            Path::new("codex"),
            &[],
            &request(
                &missing,
                &config_root.join("session-1"),
                resolved("https://provider-a.example.com", "model-a", Some("KEY_A")),
            ),
        )
        .expect_err("a missing project folder must fail");
        assert!(matches!(error, AgentError::ProjectDirectoryMissing { .. }));
        assert!(error.to_string().contains("does-not-exist"));
    }

    #[test]
    fn a_config_directory_that_cannot_be_created_fails_the_spawn_description() {
        let project = TempDir::new("codex-project");
        let blocker = TempDir::new("codex-config-blocker");
        // A *file* where the config directory's parent needs to be.
        let blocked_parent = blocker.join("blocked");
        std::fs::write(&blocked_parent, b"not a folder").unwrap();

        let error = CodexAdapter::build_spawn(
            Path::new("codex"),
            &[],
            &request(
                project.path(),
                &blocked_parent.join("session-1"),
                resolved("https://provider-a.example.com", "model-a", Some("KEY_A")),
            ),
        )
        .expect_err("an unusable config directory must fail");
        assert!(matches!(error, AgentError::ConfigDirectory { .. }));
        // No credential may appear in the message (spec section 17).
        assert!(!error.to_string().contains("KEY_A"));
    }

    /// The seam the decision "a fake program keeps the production environment"
    /// depends on: substituting the program changes nothing else.
    #[test]
    fn a_fake_program_keeps_the_production_environment_path() {
        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");

        let spawn = CodexAdapter::build_spawn(
            Path::new("cmd.exe"),
            &["/k".to_string()],
            &request(
                project.path(),
                &config_root.join("session-1"),
                resolved("https://provider-a.example.com", "model-a", Some("KEY_A")),
            ),
        )
        .unwrap();

        assert_eq!(spawn.program, PathBuf::from("cmd.exe"));
        assert_eq!(spawn.args, vec!["/k".to_string()]);
        assert_eq!(
            spawn.environment_value(ENV_BASE_URL),
            Some("https://provider-a.example.com")
        );
        assert_eq!(
            spawn.environment_value(ENV_HOME),
            Some(config_root.join("session-1").display().to_string().as_str())
        );
    }

    #[test]
    fn the_real_adapter_reports_its_own_installation_state() {
        // Machine-dependent by design, so the assertions are conditional: this
        // test never requires Codex to be installed, but when it is (a developer
        // machine), it exercises the real discovery path - including Windows'
        // `.cmd` shim for npm global installs.
        let adapter = CodexAdapter::new();
        assert_eq!(adapter.is_installed(), adapter.executable_path().is_some());

        let project = TempDir::new("codex-project");
        let config_root = TempDir::new("codex-config");
        let request = request(
            project.path(),
            &config_root.join("session-1"),
            resolved("https://provider-a.example.com", "gpt-5-codex", Some("KEY_A")),
        );

        match adapter.executable_path() {
            Some(executable) => {
                println!("discovered Codex at {}", executable.display());
                let spawn = adapter
                    .spawn_description(&request)
                    .expect("a discovered executable must produce a spawn description");
                // Discovery and the spawn description must agree.
                assert_eq!(spawn.program, executable);
                assert_eq!(
                    spawn.args,
                    vec![ARG_MODEL.to_string(), "gpt-5-codex".to_string()]
                );
                assert_eq!(spawn.cwd, project.path());
            }
            None => {
                println!("Codex is not installed on this machine");
                let error = adapter
                    .spawn_description(&request)
                    .expect_err("no CLI means no spawn description");
                assert!(matches!(error, AgentError::NotInstalled { .. }));
                assert!(error.to_string().contains("Codex"));
            }
        }
    }
}
