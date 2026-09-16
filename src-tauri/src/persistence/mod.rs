//! SQLite persistence layer (spec sections 14, 19).
//!
//! Owns the on-disk application database: provider profiles, workspace
//! metadata, and UI preferences. Schema changes go through the `user_version`
//! migration runner below so existing user data is upgraded in place rather
//! than recreated.
//!
//! # Secrets are never stored here
//!
//! Spec sections 5, 14, and 17 forbid persisting API keys in SQLite. There is
//! deliberately **no secret/API-key column in any table** in this schema, and
//! none may be added: a provider's credential lives only in the OS keyring
//! (see [`crate::secrets`]), addressed by `providers.id`. The database holds
//! non-secret metadata only, so the file can be copied, backed up, or
//! inspected without leaking credentials. A unit test in [`crate::providers`]
//! asserts that a stored keyring secret never reaches the database file, and
//! that test must be extended for every new table.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

/// File name of the application database inside the platform app-data
/// directory. The directory is resolved at runtime in [`crate::run`] through
/// Tauri's path resolver, keeping [`Storage::open`] free of Tauri types.
pub const DATABASE_FILE_NAME: &str = "ai-workspace.sqlite3";

/// Errors produced by the persistence layer.
///
/// Messages are safe to show to users and to log: they never contain secret
/// material, because secrets never reach this layer (spec section 16).
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The SQLite driver reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The database file or its parent directory could not be created.
    #[error("database file error: {0}")]
    Io(#[from] std::io::Error),

    /// Another thread panicked while holding the connection lock.
    #[error("database lock was poisoned by an earlier panic")]
    LockPoisoned,

    /// A row was expected but does not exist.
    #[error("{entity} not found: {id}")]
    NotFound {
        /// Table/entity name, e.g. `provider`.
        entity: &'static str,
        /// Primary key of the missing row.
        id: String,
    },
}

/// Convenience alias for persistence results.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Schema migrations, in order: index `i` creates schema version `i + 1`.
///
/// Rules for editing this list:
/// - Never modify or reorder an entry that has shipped; append a new one.
/// - Entries only run when the stored `user_version` is lower, so a skipped
///   entry is never applied to a database that is already newer.
const MIGRATIONS: &[&str] = &[
    // --- v1: initial schema (Milestone 2) ---------------------------------
    r#"
    -- Provider profiles (spec section 5). NOTE: there is intentionally no
    -- api_key / secret column here - see the module docs.
    CREATE TABLE IF NOT EXISTS providers (
        id             TEXT PRIMARY KEY,
        name           TEXT NOT NULL,
        base_url       TEXT NOT NULL,
        model          TEXT NOT NULL,
        extra_env_json TEXT NOT NULL DEFAULT '[]',
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL
    );

    -- Workspace metadata (spec sections 9, 14). Each row is a local project
    -- bound to an agent + provider; live process state is never persisted.
    -- NOTE: no credentials column here either.
    CREATE TABLE IF NOT EXISTS workspaces (
        id           TEXT PRIMARY KEY,
        name         TEXT NOT NULL,
        project_path TEXT NOT NULL,
        agent_id     TEXT NOT NULL,
        provider_id  TEXT NOT NULL,
        model        TEXT,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );

    -- Small key/value bag for UI preferences (spec section 14). Never store
    -- secrets here either.
    CREATE TABLE IF NOT EXISTS ui_preferences (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_workspaces_provider
        ON workspaces (provider_id);
    "#,
    // --- v2: declared context window (Claude Code unknown-model warning) -----
    //
    // A gateway can serve a model id that ships in no Claude Code catalog (the
    // user's `glm-5.3` is one). Claude Code then warns that the model is
    // undescribed and sizes auto-compact from an assumed 200k window, which the
    // user fixes with `CLAUDE_CODE_MAX_CONTEXT_TOKENS`. This column stores the
    // user's declaration so a session can set that variable.
    //
    // Nullable on purpose: `NULL` means "declare nothing", which keeps Claude
    // Code's own fallback in charge. `providers.update` writes the column on
    // every edit, so clearing the field really removes the value.
    //
    // Still no secret column - a context window is model metadata, not a
    // credential (see the module docs).
    r#"
    ALTER TABLE providers ADD COLUMN max_context_tokens INTEGER;
    "#,
];

/// Owns the SQLite connection used for all application state.
///
/// `Connection` is `Send` but not `Sync`, so it is guarded by a [`Mutex`] and
/// every access goes through [`Storage::with_conn`]. That keeps lock scopes
/// short and turns lock poisoning into an explicit error instead of a panic.
pub struct Storage {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Storage")
            .field("connection", &"<sqlite connection>")
            .finish()
    }
}

impl Storage {
    /// Open (creating if needed) the database at `path`, then migrate it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::from_connection(Connection::open(path)?)
    }

    /// Open a private in-memory database (used by tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        // Foreign keys are off by default in SQLite and must be enabled per
        // connection. The current schema has no FK constraints on purpose (a
        // workspace must survive its provider being deleted), but the pragma
        // makes any constraint added later actually enforced.
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        connection.busy_timeout(Duration::from_secs(5))?;

        let storage = Self {
            connection: Mutex::new(connection),
        };
        storage.migrate()?;
        Ok(storage)
    }

    /// Apply any pending migrations.
    pub fn migrate(&self) -> Result<()> {
        self.with_conn(|connection| {
            let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

            for (index, migration) in MIGRATIONS.iter().enumerate() {
                let version = index as i64 + 1;
                if current >= version {
                    continue;
                }
                // The migration text is a compile-time constant, never user
                // input. BEGIN/COMMIT keeps a partially applied migration out
                // of the schema, and `user_version` is written inside the same
                // transaction so a crash cannot leave a half-migrated file.
                connection.execute_batch(&format!(
                    "BEGIN;\n{migration}\nPRAGMA user_version = {version};\nCOMMIT;"
                ))?;
                log::info!("applied database migration v{version}");
            }
            Ok(())
        })
    }

    /// Current schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64> {
        self.with_conn(|connection| {
            Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
        })
    }

    /// Run `operation` with exclusive access to the connection.
    pub fn with_conn<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .connection
            .lock()
            .map_err(|_| StorageError::LockPoisoned)?;
        operation(&guard)
    }

    /// Read a UI preference (spec section 14).
    pub fn get_ui_preference(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|connection| {
            let mut statement =
                connection.prepare("SELECT value FROM ui_preferences WHERE key = ?1")?;
            let mut rows = statement.query(params![key])?;
            match rows.next()? {
                Some(row) => Ok(Some(row.get(0)?)),
                None => Ok(None),
            }
        })
    }

    /// Create or overwrite a UI preference (spec section 14).
    pub fn set_ui_preference(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|connection| {
            connection.execute(
                "INSERT INTO ui_preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

/// Current UTC time as an RFC 3339 timestamp with millisecond precision
/// (for example `2026-09-14T09:31:07.412Z`).
///
/// SQLite has no native timestamp type and the project deliberately does not
/// depend on a date crate, so `created_at`/`updated_at` are stored as
/// sortable, timezone-unambiguous UTC text.
pub fn now_rfc3339() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    format_timestamp(elapsed.as_secs(), elapsed.subsec_millis())
}

/// Format a Unix timestamp as UTC RFC 3339 text with millisecond precision.
fn format_timestamp(seconds: u64, milliseconds: u32) -> String {
    let days = (seconds / 86_400) as i64;
    let second_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{milliseconds:03}Z",
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60,
    )
}

/// Convert days since 1970-01-01 into a `(year, month, day)` civil date.
///
/// Howard Hinnant's `civil_from_days` algorithm (public domain). Exact for the
/// whole proleptic Gregorian range; only 1970..9999 is reachable here.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the year.
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = (day_of_year - (153 * march_month + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    }) as u32; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Test-only helpers shared by the module test suites.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A temporary database file that deletes itself when dropped.
    pub(crate) struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        /// Reserve a unique database path for `label`. The file itself is only
        /// created when [`TempDb::storage`] runs.
        pub(crate) fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ai-workspace-test-{label}-{}-{}.sqlite3",
                std::process::id(),
                unique_suffix(),
            ));
            Self { path }
        }

        /// Open the temporary database through the normal [`Storage`] path
        /// (schema creation and migrations included).
        pub(crate) fn storage(&self) -> Storage {
            Storage::open(&self.path).expect("open temporary database")
        }

        /// The database file's path, for tests that need to build a database by
        /// hand (see the v1 → v2 upgrade test) before opening it normally.
        pub(crate) fn path(&self) -> &std::path::Path {
            &self.path
        }

        /// Raw bytes of the database file, for tests asserting what must *not*
        /// be persisted (see [`crate::providers`]).
        ///
        /// Panics when the file cannot be read: a silently empty buffer would
        /// make a "this secret must not appear in the file" assertion pass
        /// without checking anything.
        pub(crate) fn raw_bytes(&self) -> Vec<u8> {
            std::fs::read(&self.path).expect("read temporary database file")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            // Best effort: a leftover temp file must never fail a test run.
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A temporary directory that deletes itself (and its contents) when
    /// dropped. Used by the agent/session suites, which need real project and
    /// configuration directories on disk.
    pub(crate) struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        /// Create a unique temporary directory for `label`.
        pub(crate) fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ai-workspace-test-{label}-{}-{}",
                std::process::id(),
                unique_suffix(),
            ));
            std::fs::create_dir_all(&path).expect("create temporary directory");
            Self { path }
        }

        /// The directory itself.
        pub(crate) fn path(&self) -> &std::path::Path {
            &self.path
        }

        /// A path inside the directory (not created).
        pub(crate) fn join(&self, part: &str) -> std::path::PathBuf {
            self.path.join(part)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // Best effort: a leftover temp directory must never fail a test run.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Monotonic-ish unique component for temp file names.
    pub(crate) fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TempDb;
    use super::*;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(format_timestamp(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_timestamp(1, 5), "1970-01-01T00:00:01.005Z");
        // 2023-11-14T22:13:20Z
        assert_eq!(
            format_timestamp(1_700_000_000, 42),
            "2023-11-14T22:13:20.042Z"
        );
        // Leap day, and the last second of a leap year.
        assert_eq!(
            format_timestamp(1_709_164_800, 0),
            "2024-02-29T00:00:00.000Z"
        );
        assert_eq!(
            format_timestamp(1_735_689_599, 999),
            "2024-12-31T23:59:59.999Z"
        );
    }

    #[test]
    fn now_is_utc_with_millisecond_precision() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 24, "unexpected timestamp: {now}");
        assert!(now.ends_with('Z'));
        assert_eq!(&now[4..5], "-");
        assert_eq!(&now[10..11], "T");
        assert_eq!(&now[19..20], ".");
    }

    #[test]
    fn migrations_create_the_schema_and_version_it() {
        let database = TempDb::new("migrations");
        let storage = database.storage();

        assert_eq!(storage.schema_version().unwrap(), MIGRATIONS.len() as i64);

        storage
            .with_conn(|connection| {
                for table in ["providers", "workspaces", "ui_preferences"] {
                    let count: i64 = connection.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        params![table],
                        |row| row.get(0),
                    )?;
                    assert_eq!(count, 1, "missing table {table}");
                }

                // A fresh file gets the *whole* schema, v2's column included -
                // the ALTER in migration 2 runs against the table migration 1
                // created, so there is one code path for new and old databases
                // rather than a separate "current schema" definition that could
                // drift from the migrations. (`notnull` is quoted: SQLite treats
                // it as a keyword.)
                let (column_type, not_null): (String, i64) = connection.query_row(
                    "SELECT type, \"notnull\" FROM pragma_table_info('providers')
                      WHERE name = 'max_context_tokens'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(column_type, "INTEGER");
                assert_eq!(not_null, 0, "the declared window must be nullable");
                Ok(())
            })
            .unwrap();
    }

    /// An existing version-1 database - what a user who ran the previous build
    /// has - must upgrade in place, keeping its rows.
    ///
    /// The old file is built here from the *real* v1 migration text, so this
    /// test fails if `MIGRATIONS[0]` is ever edited instead of appended to.
    #[test]
    fn an_existing_v1_database_upgrades_and_keeps_its_rows() {
        assert!(
            MIGRATIONS.len() > 1,
            "this test only proves anything when a migration follows v1"
        );

        let database = TempDb::new("migrate-v1");
        let path = database.path().to_path_buf();

        {
            let connection = Connection::open(&path).expect("create the old database");
            connection
                .execute_batch(&format!(
                    "BEGIN;\n{}\nPRAGMA user_version = 1;\nCOMMIT;",
                    MIGRATIONS[0]
                ))
                .expect("apply the v1 schema");
            connection
                .execute(
                    "INSERT INTO providers
                        (id, name, base_url, model, extra_env_json, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        "provider-a",
                        "Provider A",
                        "https://provider-a.example.com",
                        "model-a",
                        "[]",
                        "2026-09-15T00:00:00.000Z"
                    ],
                )
                .expect("insert a v1 provider row");
        }

        // Open it the way the application does: the runner must bring the file
        // to the current schema version.
        let storage = database.storage();
        assert_eq!(storage.schema_version().unwrap(), MIGRATIONS.len() as i64);

        // The v1 row is still there, and the new column reads as "not declared".
        let repository = crate::providers::ProviderRepository::new(&storage);
        let profile = repository
            .get("provider-a")
            .expect("the v1 row must survive the upgrade");
        assert_eq!(profile.name, "Provider A");
        assert_eq!(profile.model, "model-a");
        assert_eq!(profile.base_url, "https://provider-a.example.com");
        assert_eq!(
            profile.max_context_tokens, None,
            "a row written before the column existed declares no window"
        );

        // And the added column is genuinely usable on the upgraded file.
        let updated = repository
            .update(
                "provider-a",
                &crate::providers::ProviderInput {
                    name: profile.name.clone(),
                    base_url: profile.base_url.clone(),
                    model: profile.model.clone(),
                    extra_env: Vec::new(),
                    max_context_tokens: Some(1_000_000),
                },
            )
            .expect("write the new column on an upgraded database");
        assert_eq!(updated.max_context_tokens, Some(1_000_000));

        let stored: Option<i64> = storage
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT max_context_tokens FROM providers WHERE id = ?1",
                    params!["provider-a"],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(stored, Some(1_000_000));
    }

    #[test]
    fn reopening_a_database_never_re_runs_migrations() {
        let database = TempDb::new("reopen");
        {
            let storage = database.storage();
            storage.set_ui_preference("theme", "dark").unwrap();
        }

        // Second open: migrate() must be a no-op and user data must survive.
        let storage = database.storage();
        assert_eq!(storage.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(
            storage.get_ui_preference("theme").unwrap(),
            Some("dark".to_string())
        );
    }

    #[test]
    fn ui_preferences_round_trip_and_overwrite() {
        let storage = Storage::open_in_memory().unwrap();

        assert_eq!(storage.get_ui_preference("missing").unwrap(), None);
        storage.set_ui_preference("last_panel", "providers").unwrap();
        assert_eq!(
            storage.get_ui_preference("last_panel").unwrap(),
            Some("providers".to_string())
        );
        storage.set_ui_preference("last_panel", "settings").unwrap();
        assert_eq!(
            storage.get_ui_preference("last_panel").unwrap(),
            Some("settings".to_string())
        );
    }

    #[test]
    fn opening_a_database_creates_missing_directories() {
        let parent = std::env::temp_dir().join(format!(
            "ai-workspace-test-dir-{}-{}",
            std::process::id(),
            test_support::unique_suffix(),
        ));
        let path = parent.join("nested").join("app.sqlite3");

        let storage = Storage::open(&path).expect("open in a directory that does not exist yet");
        assert!(path.is_file());
        assert_eq!(storage.schema_version().unwrap(), MIGRATIONS.len() as i64);

        drop(storage);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
