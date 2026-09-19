//! Workspace management (spec sections 9, 14, 19).
//!
//! A workspace is a configured local project: a name, a project folder, the
//! agent to run, the provider/model to run it against. Two layers live here:
//!
//! - [`WorkspaceRepository`] is pure persistence over the `workspaces` table:
//!   format validation only (empty fields), so it can be covered by tests that
//!   do not need a filesystem or a provider profile.
//! - [`WorkspaceManager`] adds the application rules the UI depends on - the
//!   project folder must exist, the agent must be one this build implements,
//!   and the provider must exist (spec sections 9, 16). Commands use this layer.
//!
//! This is the spec section 4 "Workspace Manager" seam, and it is why the
//! command layer can create a workspace whose Start button will work: a stored
//! row always points at a real folder and a real provider.
//!
//! Like [`crate::providers`], neither layer touches secrets: a workspace stores
//! a `provider_id` reference, and the credential stays in the OS keyring. No
//! operation here deletes anything on disk except the configuration row -
//! removing a workspace never removes the user's project files.

use std::fmt;
use std::path::Path;

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::persistence::{now_rfc3339, Storage, StorageError};
use crate::providers::{ProviderError, ProviderRepository};

/// Convenience alias for workspace operations.
pub type Result<T> = std::result::Result<T, WorkspaceError>;

/// Errors produced by workspace operations. All messages are user-facing and
/// secret-free (spec section 16).
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The persistence layer failed.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// A provider lookup failed. The workspace manager reads the `providers`
    /// table to validate a workspace's provider reference, so that layer's
    /// errors surface here unchanged (they are already user-facing text).
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// The workspace id has no row.
    #[error("workspace not found: {0}")]
    NotFound(String),

    /// `name` was empty or whitespace only.
    #[error("workspace name must not be empty")]
    EmptyName,

    /// `project_path` was empty or whitespace only.
    #[error("project folder must not be empty")]
    EmptyProjectPath,

    /// `agent_id` was empty or whitespace only.
    #[error("an agent must be selected")]
    EmptyAgentId,

    /// `provider_id` was empty or whitespace only.
    #[error("a provider must be selected")]
    EmptyProviderId,

    /// The project folder does not exist (spec section 16: "Project directory
    /// deleted"). Worded like [`crate::agents::AgentError::ProjectDirectoryMissing`]
    /// so the UI says the same thing whether the folder vanished before the
    /// workspace was saved or after.
    #[error("the project folder does not exist: {path}")]
    ProjectPathMissing {
        /// The missing folder.
        path: String,
    },

    /// The project path exists but is a file.
    #[error("the project path is not a folder: {path}")]
    ProjectPathNotDirectory {
        /// The offending path.
        path: String,
    },

    /// The project path is relative, so it would resolve against whatever
    /// working directory the application happens to have.
    #[error("the project folder must be an absolute path: {path}")]
    ProjectPathNotAbsolute {
        /// The offending path.
        path: String,
    },

    /// `provider_id` does not match any provider profile. A workspace whose
    /// provider is gone could not start, so it is refused at creation time
    /// (spec section 16: "Invalid provider configuration").
    #[error("provider not found: {0}")]
    UnknownProvider(String),

    /// `agent_id` names an adapter this build does not implement (spec section
    /// 24: only the registered agents may be started).
    #[error("unsupported agent: {0}")]
    UnsupportedAgent(String),

    /// The UI layout preference could not be stored. Details are logged, never
    /// returned, so a database message cannot leak into the UI.
    #[error("could not save the workspace layout")]
    LayoutStorage,
}

/// A persisted workspace (spec section 9).
///
/// Serialized `camelCase` for the React frontend. Live session state (process,
/// PTY, status) is deliberately *not* part of this type and is never persisted:
/// after a restart workspaces come back as configuration only, and no agent
/// process is started automatically (spec section 14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// Stable id, used as the tab identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Absolute path of the local project folder.
    pub project_path: String,
    /// Agent adapter id, for example `claude-code`.
    pub agent_id: String,
    /// Provider profile id (keyring account for that provider's API key).
    pub provider_id: String,
    /// Per-workspace model override; `None` means "use the provider default".
    pub model: Option<String>,
}

/// Caller-supplied payload for create/update.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInput {
    /// Display name.
    pub name: String,
    /// Absolute path of the local project folder.
    pub project_path: String,
    /// Agent adapter id.
    pub agent_id: String,
    /// Provider profile id.
    pub provider_id: String,
    /// Optional per-workspace model override.
    #[serde(default)]
    pub model: Option<String>,
}

impl WorkspaceInput {
    /// Validate the payload before it is persisted (spec section 16).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(WorkspaceError::EmptyName);
        }
        if self.project_path.trim().is_empty() {
            return Err(WorkspaceError::EmptyProjectPath);
        }
        if self.agent_id.trim().is_empty() {
            return Err(WorkspaceError::EmptyAgentId);
        }
        if self.provider_id.trim().is_empty() {
            return Err(WorkspaceError::EmptyProviderId);
        }
        Ok(())
    }
}

/// CRUD over the `workspaces` table (spec sections 9, 14).
pub struct WorkspaceRepository<'storage> {
    storage: &'storage Storage,
}

impl<'storage> WorkspaceRepository<'storage> {
    /// Wrap the application database.
    pub fn new(storage: &'storage Storage) -> Self {
        Self { storage }
    }

    /// Columns selected for every workspace read.
    const COLUMNS: &'static str = "id, name, project_path, agent_id, provider_id, model";

    /// Insert a new workspace and return it. The id is generated here.
    pub fn create(&self, input: &WorkspaceInput) -> Result<Workspace> {
        input.validate()?;

        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();

        self.storage.with_conn(|connection| {
            connection.execute(
                "INSERT INTO workspaces
                     (id, name, project_path, agent_id, provider_id, model, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id,
                    input.name.trim(),
                    input.project_path.trim(),
                    input.agent_id.trim(),
                    input.provider_id.trim(),
                    normalized_model(&input.model),
                    now
                ],
            )?;
            Ok(())
        })?;

        self.get(&id)
    }

    /// Load one workspace, failing when the id is unknown.
    pub fn get(&self, id: &str) -> Result<Workspace> {
        self.find(id)?
            .ok_or_else(|| WorkspaceError::NotFound(id.to_string()))
    }

    /// Load one workspace, returning `None` when the id is unknown.
    pub fn find(&self, id: &str) -> Result<Option<Workspace>> {
        let workspace = self.storage.with_conn(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {} FROM workspaces WHERE id = ?1",
                Self::COLUMNS
            ))?;
            let mut rows = statement.query(params![id])?;
            match rows.next()? {
                Some(row) => Ok(Some(row_to_workspace(row)?)),
                None => Ok(None),
            }
        })?;
        Ok(workspace)
    }

    /// List all workspaces in creation order - the order tabs are reopened in
    /// after a restart (spec sections 10, 14).
    pub fn list(&self) -> Result<Vec<Workspace>> {
        let workspaces = self.storage.with_conn(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {} FROM workspaces ORDER BY created_at ASC, name COLLATE NOCASE ASC",
                Self::COLUMNS
            ))?;
            let workspaces = statement
                .query_map([], row_to_workspace)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(workspaces)
        })?;
        Ok(workspaces)
    }

    /// Rename/repoint a workspace. `created_at` is preserved and `updated_at`
    /// is refreshed.
    pub fn update(&self, id: &str, input: &WorkspaceInput) -> Result<Workspace> {
        input.validate()?;

        let now = now_rfc3339();

        let updated = self.storage.with_conn(|connection| {
            let updated = connection.execute(
                "UPDATE workspaces
                    SET name = ?2, project_path = ?3, agent_id = ?4, provider_id = ?5,
                        model = ?6, updated_at = ?7
                  WHERE id = ?1",
                params![
                    id,
                    input.name.trim(),
                    input.project_path.trim(),
                    input.agent_id.trim(),
                    input.provider_id.trim(),
                    normalized_model(&input.model),
                    now
                ],
            )?;
            Ok(updated)
        })?;

        // No matching row: report it the same way `get` does. The zero-row check
        // stays outside the storage closure so the error is a
        // `WorkspaceError::NotFound` rather than a wrapped `StorageError` - the
        // caller must be able to tell "this workspace is gone" apart from "the
        // database failed".
        if updated == 0 {
            return Err(WorkspaceError::NotFound(id.to_string()));
        }

        self.get(id)
    }

    /// Remove a workspace from the application (spec section 9: "Remove a
    /// workspace from the application"). Returns `false` when it was already
    /// gone. The project folder on disk is never touched.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let deleted = self.storage.with_conn(|connection| {
            let deleted = connection.execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
            Ok(deleted > 0)
        })?;
        Ok(deleted)
    }
}

/// `ui_preferences` key holding the last open tab set (spec section 14).
pub const LAYOUT_PREFERENCE_KEY: &str = "workspace_layout";

/// Upper bound on how many open tabs are remembered, so a corrupt or hostile
/// preference value cannot grow the row without limit.
const MAX_LAYOUT_ENTRIES: usize = 64;

/// The UI half of workspace state: which workspace tabs were open, and which
/// one was active (spec sections 10, 14).
///
/// This is a *preference*, not configuration: it lives in `ui_preferences` as
/// JSON, and no agent process is started from it. After a restart the tabs come
/// back with stopped sessions, and the user presses Start (spec section 14:
/// "Do not automatically start every previous agent process").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    /// Open tab order; ids that no longer exist are ignored when restoring.
    #[serde(default)]
    pub open_workspace_ids: Vec<String>,
    /// The tab that was active, if it is still open.
    #[serde(default)]
    pub active_workspace_id: Option<String>,
}

/// Workspace rules on top of [`WorkspaceRepository`] (spec sections 4, 9, 16).
///
/// Every write goes through [`WorkspaceManager::validate`], so a persisted
/// workspace is always startable: real project folder, implemented agent, and a
/// provider profile that exists.
pub struct WorkspaceManager<'storage> {
    storage: &'storage Storage,
}

impl<'storage> WorkspaceManager<'storage> {
    /// Wrap the application database.
    pub fn new(storage: &'storage Storage) -> Self {
        Self { storage }
    }

    /// The persistence layer, for reads that need no validation.
    fn repository(&self) -> WorkspaceRepository<'storage> {
        WorkspaceRepository::new(self.storage)
    }

    /// Every configured workspace, in creation order (spec section 10: the list
    /// tabs are reopened from).
    pub fn list(&self) -> Result<Vec<Workspace>> {
        self.repository().list()
    }

    /// Load one workspace, failing when the id is unknown.
    pub fn get(&self, id: &str) -> Result<Workspace> {
        self.repository().get(id)
    }

    /// Load one workspace, returning `None` when the id is unknown.
    pub fn find(&self, id: &str) -> Result<Option<Workspace>> {
        self.repository().find(id)
    }

    /// Create a workspace after validating it (spec sections 9, 16).
    pub fn create(&self, input: &WorkspaceInput) -> Result<Workspace> {
        self.validate(input)?;
        self.repository().create(input)
    }

    /// Rename a workspace and/or change its project folder, agent, provider or
    /// model. The same validation as [`WorkspaceManager::create`] applies, so an
    /// edit cannot leave a workspace pointing at a folder that is gone.
    pub fn update(&self, id: &str, input: &WorkspaceInput) -> Result<Workspace> {
        self.validate(input)?;
        self.repository().update(id, input)
    }

    /// Remove a workspace from the application.
    ///
    /// The workspace's **configuration only**: the project folder and every file
    /// in it are left untouched, as are the provider profiles and the keyring
    /// entries they use (spec sections 9, 14). Returns `false` when the row was
    /// already gone.
    ///
    /// Any live session for the workspace is the caller's business (the command
    /// layer stops it) - this layer has no process knowledge.
    pub fn delete(&self, id: &str) -> Result<bool> {
        // Deliberately just the row. Kept as an explicit method (rather than
        // letting callers reach the repository) so "delete never cascades" has
        // one place to read and one place to test.
        self.repository().delete(id)
    }

    /// The remembered tab set, or the default when nothing was stored yet.
    ///
    /// An unreadable value is *not* an error: a preference that cannot be parsed
    /// must degrade to "no tabs remembered" rather than stopping the app from
    /// starting (spec section 16).
    pub fn load_layout(&self) -> Result<WorkspaceLayout> {
        let Some(raw) = self
            .storage
            .get_ui_preference(LAYOUT_PREFERENCE_KEY)
            .map_err(WorkspaceError::Storage)?
        else {
            return Ok(WorkspaceLayout::default());
        };
        match serde_json::from_str::<WorkspaceLayout>(&raw) {
            Ok(layout) => Ok(layout.normalized()),
            Err(error) => {
                log::warn!("ignoring an unreadable workspace layout preference: {error}");
                Ok(WorkspaceLayout::default())
            }
        }
    }

    /// Remember the tab set (spec section 14).
    ///
    /// The value is normalized first: duplicate and blank ids are dropped, the
    /// list is capped, and an active id that is not open becomes `None`, so what
    /// the frontend reads back is always self-consistent.
    pub fn save_layout(&self, layout: &WorkspaceLayout) -> Result<WorkspaceLayout> {
        let layout = layout.normalized();
        let encoded = serde_json::to_string(&layout).map_err(|error| {
            // Only ever a serialization failure of plain strings/`None`; the
            // detail goes to the log and never to the UI (spec section 17).
            log::warn!("could not encode the workspace layout: {error}");
            WorkspaceError::LayoutStorage
        })?;
        self.storage
            .set_ui_preference(LAYOUT_PREFERENCE_KEY, &encoded)
            .map_err(WorkspaceError::Storage)?;
        Ok(layout)
    }

    /// Validate every rule a persisted workspace must satisfy (spec section 16).
    fn validate(&self, input: &WorkspaceInput) -> Result<()> {
        // Format first: empty fields are reported as themselves rather than as
        // "the project folder does not exist" for an empty path.
        input.validate()?;
        validate_project_path(&input.project_path)?;
        validate_agent(&input.agent_id)?;
        self.validate_provider(&input.provider_id)
    }

    /// The provider must exist, otherwise the workspace could never start.
    fn validate_provider(&self, provider_id: &str) -> Result<()> {
        let provider_id = provider_id.trim();
        let exists = ProviderRepository::new(self.storage)
            .find(provider_id)?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(WorkspaceError::UnknownProvider(provider_id.to_string()))
        }
    }
}

impl fmt::Debug for WorkspaceManager<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceManager")
            .finish_non_exhaustive()
    }
}

impl WorkspaceLayout {
    /// Drop blank and duplicate ids, cap the list, and point `active` at an open
    /// id only.
    fn normalized(&self) -> WorkspaceLayout {
        let mut seen = Vec::new();
        for id in &self.open_workspace_ids {
            let id = id.trim();
            if id.is_empty() || seen.iter().any(|kept: &String| kept == id) {
                continue;
            }
            seen.push(id.to_string());
            if seen.len() == MAX_LAYOUT_ENTRIES {
                break;
            }
        }

        let active = self
            .active_workspace_id
            .as_ref()
            .map(|id| id.trim())
            .filter(|id| seen.iter().any(|kept| kept == id))
            .map(str::to_string);

        WorkspaceLayout {
            open_workspace_ids: seen,
            active_workspace_id: active,
        }
    }
}

/// The project folder must be an absolute path to a directory that exists.
fn validate_project_path(project_path: &str) -> Result<()> {
    let project_path = project_path.trim();
    let path = Path::new(project_path);

    // Checked before existence so a relative path is reported as such instead of
    // silently resolving against the application's working directory.
    if !path.is_absolute() {
        return Err(WorkspaceError::ProjectPathNotAbsolute {
            path: project_path.to_string(),
        });
    }
    if !path.exists() {
        return Err(WorkspaceError::ProjectPathMissing {
            path: project_path.to_string(),
        });
    }
    if !path.is_dir() {
        return Err(WorkspaceError::ProjectPathNotDirectory {
            path: project_path.to_string(),
        });
    }
    Ok(())
}

/// Only an agent this build implements may be persisted (spec sections 6, 24);
/// the message names the id so an adapter's absence is obvious.
///
/// The set of implemented agents is owned by [`crate::agents`], so registering a
/// new adapter there is all it takes for workspaces to accept it - the two can
/// never disagree about which ids are real.
fn validate_agent(agent_id: &str) -> Result<()> {
    if crate::agents::is_supported(agent_id) {
        Ok(())
    } else {
        Err(WorkspaceError::UnsupportedAgent(agent_id.trim().to_string()))
    }
}

/// Map a `workspaces` row (in [`WorkspaceRepository::COLUMNS`] order) to a
/// workspace.
fn row_to_workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get("id")?,
        name: row.get("name")?,
        project_path: row.get("project_path")?,
        agent_id: row.get("agent_id")?,
        provider_id: row.get("provider_id")?,
        model: row.get("model")?,
    })
}

/// Treat a blank model override as "no override" so the column holds either a
/// real model id or NULL.
fn normalized_model(model: &Option<String>) -> Option<String> {
    model
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

impl<'storage> fmt::Debug for WorkspaceRepository<'storage> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceRepository")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::AGENT_ID;
    use crate::persistence::test_support::TempDb;

    fn input(name: &str) -> WorkspaceInput {
        WorkspaceInput {
            name: name.to_string(),
            project_path: format!("D:\\Work\\{name}"),
            agent_id: "claude-code".to_string(),
            provider_id: "provider-a-id".to_string(),
            model: None,
        }
    }

    #[test]
    fn workspace_metadata_round_trip() {
        let database = TempDb::new("workspace-round-trip");
        let storage = database.storage();
        let repository = WorkspaceRepository::new(&storage);

        assert!(repository.list().unwrap().is_empty());

        let created = repository
            .create(&WorkspaceInput {
                model: Some("model-a".to_string()),
                ..input("project-alpha")
            })
            .unwrap();
        assert!(!created.id.is_empty());
        assert_eq!(created.name, "project-alpha");
        assert_eq!(created.project_path, "D:\\Work\\project-alpha");
        assert_eq!(created.agent_id, "claude-code");
        assert_eq!(created.provider_id, "provider-a-id");
        assert_eq!(created.model, Some("model-a".to_string()));

        // Read back through a fresh repository over the same storage.
        assert_eq!(
            WorkspaceRepository::new(&storage).get(&created.id).unwrap(),
            created
        );

        // A workspaces metadata row must also survive reopening the file, which
        // is the "restore configured workspaces after restart" requirement.
        drop(repository);
        drop(storage);
        let reopened = database.storage();
        let repository = WorkspaceRepository::new(&reopened);
        let restored = repository.list().unwrap();
        assert_eq!(restored, vec![created.clone()]);

        // Rename + repoint provider; `model: None` clears the override.
        let updated = repository
            .update(
                &created.id,
                &WorkspaceInput {
                    name: "project-alpha-renamed".to_string(),
                    project_path: created.project_path.clone(),
                    agent_id: created.agent_id.clone(),
                    provider_id: "provider-b-id".to_string(),
                    model: None,
                },
            )
            .unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "project-alpha-renamed");
        assert_eq!(updated.provider_id, "provider-b-id");
        assert_eq!(updated.model, None);
        assert_eq!(updated.project_path, created.project_path);

        // Deleting is idempotent from the caller's perspective.
        assert!(repository.delete(&created.id).unwrap());
        assert!(!repository.delete(&created.id).unwrap());
        assert!(matches!(
            repository.get(&created.id),
            Err(WorkspaceError::NotFound(_))
        ));
        assert!(repository.list().unwrap().is_empty());
    }

    #[test]
    fn updating_an_unknown_workspace_reports_not_found() {
        // Regression: a missing row must surface as `WorkspaceError::NotFound`,
        // not as a wrapped `StorageError::NotFound`. Callers (the session
        // command layer included) match on the module error to tell "this
        // workspace is gone" apart from "the database failed".
        let database = TempDb::new("workspace-update-missing");
        let storage = database.storage();
        let repository = WorkspaceRepository::new(&storage);

        let error = repository
            .update("does-not-exist", &input("project-alpha"))
            .expect_err("updating a missing workspace must fail");
        assert!(
            matches!(error, WorkspaceError::NotFound(ref id) if id == "does-not-exist"),
            "expected WorkspaceError::NotFound, got {error:?}"
        );
        // The message names the workspace, as `get` does; no database detail.
        assert_eq!(error.to_string(), "workspace not found: does-not-exist");
        assert!(
            !matches!(error, WorkspaceError::Storage(_)),
            "a missing row must not be reported as a storage failure"
        );
    }

    #[test]
    fn multiple_workspaces_stay_independent() {
        // The persistence half of spec section 8: each workspace keeps its own
        // project, agent, provider, and model.
        let database = TempDb::new("workspace-isolation");
        let storage = database.storage();
        let repository = WorkspaceRepository::new(&storage);

        let alpha = repository
            .create(&WorkspaceInput {
                name: "Project Alpha".to_string(),
                project_path: "D:\\Work\\project-alpha".to_string(),
                agent_id: "claude-code".to_string(),
                provider_id: "provider-a-id".to_string(),
                model: Some("model-a".to_string()),
            })
            .unwrap();
        let beta = repository
            .create(&WorkspaceInput {
                name: "Project Beta".to_string(),
                project_path: "D:\\Work\\project-beta".to_string(),
                agent_id: "claude-code".to_string(),
                provider_id: "provider-b-id".to_string(),
                model: Some("model-b".to_string()),
            })
            .unwrap();

        assert_ne!(alpha.id, beta.id);
        assert_eq!(repository.list().unwrap().len(), 2);

        // Changing one workspace leaves the other untouched.
        repository
            .update(
                &alpha.id,
                &WorkspaceInput {
                    name: alpha.name.clone(),
                    project_path: alpha.project_path.clone(),
                    agent_id: alpha.agent_id.clone(),
                    provider_id: "provider-c-id".to_string(),
                    model: alpha.model.clone(),
                },
            )
            .unwrap();
        let beta_after = repository.get(&beta.id).unwrap();
        assert_eq!(beta_after.provider_id, "provider-b-id");
        assert_eq!(beta_after.model, Some("model-b".to_string()));
    }

    #[test]
    fn blank_model_override_is_stored_as_null() {
        let database = TempDb::new("workspace-blank-model");
        let storage = database.storage();
        let repository = WorkspaceRepository::new(&storage);

        let created = repository
            .create(&WorkspaceInput {
                model: Some("   ".to_string()),
                ..input("project-alpha")
            })
            .unwrap();
        assert_eq!(created.model, None);

        let stored: Option<String> = storage
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT model FROM workspaces WHERE id = ?1",
                    params![created.id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(stored, None);
    }

    #[test]
    fn validation_rejects_incomplete_workspaces() {
        let database = TempDb::new("workspace-validation");
        let storage = database.storage();
        let repository = WorkspaceRepository::new(&storage);

        assert!(matches!(
            repository.create(&WorkspaceInput {
                name: "  ".to_string(),
                ..input("ignored")
            }),
            Err(WorkspaceError::EmptyName)
        ));
        assert!(matches!(
            repository.create(&WorkspaceInput {
                project_path: String::new(),
                ..input("project-alpha")
            }),
            Err(WorkspaceError::EmptyProjectPath)
        ));
        assert!(matches!(
            repository.create(&WorkspaceInput {
                agent_id: String::new(),
                ..input("project-alpha")
            }),
            Err(WorkspaceError::EmptyAgentId)
        ));
        assert!(matches!(
            repository.create(&WorkspaceInput {
                provider_id: String::new(),
                ..input("project-alpha")
            }),
            Err(WorkspaceError::EmptyProviderId)
        ));

        assert!(repository.list().unwrap().is_empty());
    }

    // --- Workspace Manager (Milestone 4) ------------------------------------
    //
    // These cover the rules that make a persisted workspace *startable*: the
    // project folder exists, the agent is implemented, and the provider profile
    // is real. The repository tests above intentionally stay filesystem-free.

    use crate::persistence::test_support::TempDir;
    use crate::providers::{ProviderInput, ProviderRepository};

    /// Create a provider profile and return its id.
    fn provider(storage: &Storage, label: &str) -> String {
        ProviderRepository::new(storage)
            .create(&ProviderInput {
                name: format!("Provider {label}"),
                base_url: format!("https://provider-{label}.example.com"),
                model: format!("model-{label}"),
                extra_env: Vec::new(),
                max_context_tokens: None,
            })
            .expect("create a provider profile")
            .id
    }

    /// A valid create payload for `project_path`, bound to `provider_id`.
    fn manager_input(project_path: &Path, provider_id: &str) -> WorkspaceInput {
        WorkspaceInput {
            name: "Project Alpha".to_string(),
            project_path: project_path.display().to_string(),
            agent_id: AGENT_ID.to_string(),
            provider_id: provider_id.to_string(),
            model: None,
        }
    }

    #[test]
    fn the_manager_creates_and_updates_a_startable_workspace() {
        let database = TempDb::new("manager-round-trip");
        let storage = database.storage();
        let project = TempDir::new("manager-project");
        let provider_a = provider(&storage, "a");
        let provider_b = provider(&storage, "b");

        let manager = WorkspaceManager::new(&storage);
        let created = manager
            .create(&WorkspaceInput {
                model: Some("model-a".to_string()),
                ..manager_input(project.path(), &provider_a)
            })
            .expect("create a workspace for an existing folder and provider");

        assert_eq!(created.project_path, project.path().display().to_string());
        assert_eq!(created.agent_id, AGENT_ID);
        assert_eq!(created.model, Some("model-a".to_string()));
        assert_eq!(manager.list().unwrap(), vec![created.clone()]);

        // Rename, repoint the provider, change the model; the same validation
        // applies to an update, so an edit cannot break startability.
        let updated = manager
            .update(
                &created.id,
                &WorkspaceInput {
                    name: "Project Alpha Renamed".to_string(),
                    model: Some("model-b".to_string()),
                    ..manager_input(project.path(), &provider_b)
                },
            )
            .expect("update a workspace");
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "Project Alpha Renamed");
        assert_eq!(updated.provider_id, provider_b);
        assert_eq!(updated.model, Some("model-b".to_string()));

        // Updating a workspace that is gone is reported as such, and the same
        // rules still run for the payload it was given.
        assert!(matches!(
            manager.update("does-not-exist", &manager_input(project.path(), &provider_a)),
            Err(WorkspaceError::NotFound(_))
        ));
    }

    /// Every agent the registry implements is persistable (spec sections 6, 9,
    /// 24), so choosing Codex in the workspace form stores a row that a session
    /// can actually start - the persistence half of "a user can create and run a
    /// Codex session".
    #[test]
    fn every_supported_agent_can_be_persisted_and_listed_back() {
        let database = TempDb::new("manager-agents");
        let storage = database.storage();
        let project = TempDir::new("manager-agents-project");
        let provider_id = provider(&storage, "a");
        let manager = WorkspaceManager::new(&storage);

        for (index, agent_id) in crate::agents::AGENT_IDS.iter().enumerate() {
            let created = manager
                .create(&WorkspaceInput {
                    name: format!("Project {index}"),
                    agent_id: (*agent_id).to_string(),
                    ..manager_input(project.path(), &provider_id)
                })
                .unwrap_or_else(|error| panic!("{agent_id} must be creatable: {error}"));
            assert_eq!(created.agent_id, *agent_id);
        }

        // Both persisted workspaces are listed back with their own agent, which
        // is what lets sessions with different agents coexist (spec section 9).
        let listed: Vec<String> = manager
            .list()
            .expect("list workspaces")
            .into_iter()
            .map(|workspace| workspace.agent_id)
            .collect();
        for agent_id in crate::agents::AGENT_IDS {
            assert!(
                listed.iter().any(|listed_id| listed_id == agent_id),
                "{agent_id} was persisted but not listed: {listed:?}"
            );
        }
    }

    #[test]
    fn the_manager_rejects_a_project_folder_that_is_not_a_directory() {
        let database = TempDb::new("manager-project-path");
        let storage = database.storage();
        let project = TempDir::new("manager-missing-project");
        let provider_id = provider(&storage, "a");
        let manager = WorkspaceManager::new(&storage);

        // (a) Does not exist.
        let missing = project.join("deleted");
        let error = manager
            .create(&manager_input(&missing, &provider_id))
            .expect_err("a missing project folder must be refused");
        assert!(
            matches!(error, WorkspaceError::ProjectPathMissing { ref path } if path == &missing.display().to_string()),
            "expected ProjectPathMissing, got {error:?}"
        );
        assert!(error.to_string().starts_with("the project folder does not exist:"));
        assert!(manager.list().unwrap().is_empty());

        // (b) Exists, but is a file.
        let file = project.join("not-a-folder.txt");
        std::fs::write(&file, b"x").expect("write a test file");
        let error = manager
            .create(&manager_input(&file, &provider_id))
            .expect_err("a file is not a project folder");
        assert!(matches!(error, WorkspaceError::ProjectPathNotDirectory { .. }));
        assert!(error.to_string().starts_with("the project path is not a folder:"));

        // (c) Relative paths are refused rather than resolved against whatever
        // working directory the application happens to have.
        let error = manager
            .create(&manager_input(Path::new("some-relative-folder"), &provider_id))
            .expect_err("a relative project folder must be refused");
        assert!(matches!(error, WorkspaceError::ProjectPathNotAbsolute { .. }));

        // Nothing was persisted by any of the failed attempts.
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn the_manager_rejects_an_unknown_provider_or_agent() {
        let database = TempDb::new("manager-provider-agent");
        let storage = database.storage();
        let project = TempDir::new("manager-provider-project");
        let provider_id = provider(&storage, "a");
        let manager = WorkspaceManager::new(&storage);

        // Provider id that has no profile: the workspace could never start
        // (spec section 16: "Invalid provider configuration").
        let error = manager
            .create(&manager_input(project.path(), "no-such-provider"))
            .expect_err("an unknown provider must be refused");
        assert!(matches!(error, WorkspaceError::UnknownProvider(ref id) if id == "no-such-provider"));
        assert_eq!(error.to_string(), "provider not found: no-such-provider");

        // An agent this build does not implement (spec section 24). Codex is a
        // *supported* agent since it was added alongside Claude Code, so this
        // uses an id no adapter claims.
        let error = manager
            .create(&WorkspaceInput {
                agent_id: "gemini".to_string(),
                ..manager_input(project.path(), &provider_id)
            })
            .expect_err("an unimplemented agent must be refused");
        assert!(matches!(error, WorkspaceError::UnsupportedAgent(ref id) if id == "gemini"));
        assert_eq!(error.to_string(), "unsupported agent: gemini");

        // Empty fields are reported as themselves, before the filesystem and
        // provider checks, so the message names what the user must fix.
        let error = manager
            .create(&WorkspaceInput {
                name: "   ".to_string(),
                ..manager_input(&project.join("does-not-exist"), &provider_id)
            })
            .expect_err("an empty name must be refused");
        assert!(matches!(error, WorkspaceError::EmptyName));
        assert_eq!(error.to_string(), "workspace name must not be empty");

        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn deleting_a_workspace_removes_configuration_only() {
        // Spec sections 9, 14: removing a workspace from the application must
        // not delete the user's project files, and must not cascade into the
        // provider profiles or their credentials.
        let database = TempDb::new("manager-delete");
        let storage = database.storage();
        let project = TempDir::new("manager-delete-project");
        let provider_id = provider(&storage, "a");

        // A file inside the project folder, to prove the folder is untouched.
        let marker = project.join("marker.txt");
        std::fs::write(&marker, b"user data").expect("write a marker file");

        let manager = WorkspaceManager::new(&storage);
        let workspace = manager
            .create(&manager_input(project.path(), &provider_id))
            .expect("create a workspace");

        assert!(manager.delete(&workspace.id).expect("delete the workspace"));
        assert!(
            matches!(manager.get(&workspace.id), Err(WorkspaceError::NotFound(_))),
            "the configuration row must be gone"
        );
        assert!(manager.list().unwrap().is_empty());
        // Deleting twice is harmless.
        assert!(!manager.delete(&workspace.id).unwrap());

        // The project folder and its contents survive.
        assert!(project.path().is_dir(), "the project folder must survive");
        assert!(marker.is_file(), "project files must survive");
        assert_eq!(std::fs::read(&marker).unwrap(), b"user data");

        // The provider profile survives, so the workspace can be recreated.
        assert!(ProviderRepository::new(&storage).find(&provider_id).unwrap().is_some());
    }

    #[test]
    fn deleting_a_provider_does_not_cascade_into_workspaces() {
        // The other direction: spec section 9 keeps workspace configuration when
        // a provider is removed, so the workspace can be repointed. It just
        // cannot be *started* or *updated* until a real provider is chosen.
        let database = TempDb::new("manager-provider-delete");
        let storage = database.storage();
        let project = TempDir::new("manager-provider-delete-project");
        let provider_id = provider(&storage, "a");

        let manager = WorkspaceManager::new(&storage);
        let workspace = manager
            .create(&manager_input(project.path(), &provider_id))
            .unwrap();

        ProviderRepository::new(&storage)
            .delete(&provider_id)
            .expect("delete the provider profile");

        // Still listed, still readable.
        assert_eq!(manager.list().unwrap(), vec![workspace.clone()]);
        assert_eq!(manager.get(&workspace.id).unwrap(), workspace);

        // Editing it now fails, because the provider it points at is gone.
        let error = manager
            .update(&workspace.id, &manager_input(project.path(), &provider_id))
            .expect_err("a workspace cannot be saved against a deleted provider");
        assert!(matches!(error, WorkspaceError::UnknownProvider(_)));

        // ... and repointing it at a live provider works again.
        let replacement = provider(&storage, "b");
        let repaired = manager
            .update(&workspace.id, &manager_input(project.path(), &replacement))
            .unwrap();
        assert_eq!(repaired.provider_id, replacement);
        assert_eq!(repaired.id, workspace.id, "the id and its tab survive an edit");
    }

    #[test]
    fn the_tab_layout_round_trips_and_is_normalized() {
        let database = TempDb::new("manager-layout");
        let storage = database.storage();
        let manager = WorkspaceManager::new(&storage);

        // Nothing stored yet.
        assert_eq!(manager.load_layout().unwrap(), WorkspaceLayout::default());

        // Duplicates, blanks and an active id that is not open are cleaned up,
        // so the frontend never restores a tab set that contradicts itself.
        let saved = manager
            .save_layout(&WorkspaceLayout {
                open_workspace_ids: vec![
                    "alpha".to_string(),
                    "  ".to_string(),
                    "beta".to_string(),
                    "alpha".to_string(),
                ],
                active_workspace_id: Some("gamma".to_string()),
            })
            .expect("save the layout");
        assert_eq!(
            saved.open_workspace_ids,
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert_eq!(saved.active_workspace_id, None);

        // The normalized value is what a later load returns.
        assert_eq!(manager.load_layout().unwrap(), saved);

        // A consistent layout keeps its active tab, and survives a restart
        // (a fresh manager over the same database file).
        let consistent = manager
            .save_layout(&WorkspaceLayout {
                open_workspace_ids: vec!["alpha".to_string(), "beta".to_string()],
                active_workspace_id: Some("beta".to_string()),
            })
            .unwrap();
        assert_eq!(consistent.active_workspace_id, Some("beta".to_string()));

        drop(manager);
        drop(storage);
        let reopened = database.storage();
        assert_eq!(
            WorkspaceManager::new(&reopened).load_layout().unwrap(),
            consistent
        );
    }

    #[test]
    fn an_unreadable_tab_layout_does_not_stop_the_application() {
        // A corrupt preference must degrade to "nothing remembered" instead of
        // failing the load (spec section 16: errors must not break the app).
        let database = TempDb::new("manager-layout-corrupt");
        let storage = database.storage();
        storage
            .set_ui_preference(LAYOUT_PREFERENCE_KEY, "{ not json")
            .unwrap();

        let manager = WorkspaceManager::new(&storage);
        assert_eq!(manager.load_layout().unwrap(), WorkspaceLayout::default());

        // Storing a good value over it recovers.
        let saved = manager
            .save_layout(&WorkspaceLayout {
                open_workspace_ids: vec!["alpha".to_string()],
                active_workspace_id: Some("alpha".to_string()),
            })
            .unwrap();
        assert_eq!(manager.load_layout().unwrap(), saved);
    }

    #[test]
    fn workspace_payloads_are_camel_case_and_match_the_frontend_types() {
        // The wire contract `src/types/index.ts` declares. A rename here must
        // fail this test rather than silently blanking the React UI.
        let workspace = Workspace {
            id: "workspace-1".to_string(),
            name: "Project Alpha".to_string(),
            project_path: "D:\\Work\\project-alpha".to_string(),
            agent_id: AGENT_ID.to_string(),
            provider_id: "provider-a".to_string(),
            model: Some("model-a".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&workspace).unwrap(),
            serde_json::json!({
                "id": "workspace-1",
                "name": "Project Alpha",
                "projectPath": "D:\\Work\\project-alpha",
                "agentId": "claude-code",
                "providerId": "provider-a",
                "model": "model-a",
            })
        );

        // A workspace without a model override serializes `null`, which is what
        // the frontend's `string | null` expects.
        let without_model = Workspace {
            model: None,
            ..workspace
        };
        assert_eq!(
            serde_json::to_value(&without_model).unwrap()["model"],
            serde_json::Value::Null
        );

        // The layout preference the frontend saves and loads back.
        assert_eq!(
            serde_json::to_value(WorkspaceLayout {
                open_workspace_ids: vec!["workspace-1".to_string()],
                active_workspace_id: Some("workspace-1".to_string()),
            })
            .unwrap(),
            serde_json::json!({
                "openWorkspaceIds": ["workspace-1"],
                "activeWorkspaceId": "workspace-1",
            })
        );
    }
}
