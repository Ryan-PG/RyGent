/** Tab-level status; drives the status dot in `TabBar` and the footer text. */
export type SessionStatus = "idle" | "running" | "exited" | "failed";

/**
 * A persisted workspace: a configured local project (spec section 9).
 *
 * Mirrors `crate::workspaces::Workspace`. It carries a provider *reference*
 * (`providerId`), never a credential - the API key stays in the OS keyring.
 */
export interface Workspace {
  id: string;
  /** Display name; also the tab title. */
  name: string;
  /** Absolute path of the project folder. */
  projectPath: string;
  /** Agent adapter id, e.g. `claude-code`. */
  agentId: string;
  /** Provider profile id. */
  providerId: string;
  /** Per-workspace model override; `null` means "use the provider default". */
  model: string | null;
}

/** Payload for creating or updating a workspace (no secret, no id). */
export interface WorkspaceInput {
  name: string;
  projectPath: string;
  agentId: string;
  providerId: string;
  model: string | null;
}

/**
 * The remembered tab set (spec section 14).
 *
 * A UI preference: restoring it opens tabs, never agent processes.
 */
export interface WorkspaceLayout {
  openWorkspaceIds: string[];
  activeWorkspaceId: string | null;
}

/**
 * One open tab: a workspace projected into the shell.
 *
 * `id` **is** the workspace id. There is exactly one tab per workspace, for the
 * same reason the backend allows only one live session per workspace - opening
 * the same project twice would mean two agents fighting over one directory.
 * Denormalized fields are refreshed from the workspace (and the providers list)
 * whenever either changes, so the tab bar and the status bar never show stale
 * metadata.
 */
export interface WorkspaceTab {
  id: string;
  workspaceId: string;
  title: string;
  projectPath: string;
  /**
   * Agent display label, e.g. `Claude Code`, resolved from the loaded agent list
   * (see `agentLabel` in the store) so the UI says which CLI a tab runs.
   */
  agent: string;
  /** Provider profile id; the display name is resolved from the providers list. */
  provider: string;
  model: string;
}

/**
 * Lifecycle of the PTY-backed agent session behind one tab (spec sections 8,
 * 11, 15).
 *
 * Distinct from `SessionStatus`: this is the real session state machine
 * reported by the Rust session manager, while `SessionStatus` is the coarse
 * "what colour is the dot" summary. `tabStatusFromSession` maps one onto the
 * other.
 */
export type TerminalSessionStatus =
  | "created"
  | "starting"
  | "running"
  | "stopping"
  | "stopped"
  | "failed";

/** Per-tab session state held in the zustand store. */
export interface TerminalSession {
  /** Backend session id; absent until `start_session` has succeeded. */
  id?: string;
  status: TerminalSessionStatus;
  /** Last size the terminal reported, forwarded to the PTY. */
  cols: number;
  rows: number;
  /** Last user-facing backend message (spawn failure, exit code). */
  message?: string;
}

/** A session row as returned by the Rust session manager. */
export interface SessionInfo {
  id: string;
  workspaceId: string;
  status: TerminalSessionStatus;
  cols: number;
  rows: number;
  /** Present once the process exited; `null` while it lives. */
  exitCode: number | null;
}

/** Payload of the `session-state:<id>` event. */
export interface SessionStateEvent {
  sessionId: string;
  status: TerminalSessionStatus;
  exitCode: number | null;
}

/**
 * A provider's extra environment variable, as stored by the Rust core: a JSON
 * two-element array `[name, value]` (spec section 5).
 */
export type EnvironmentPair = [string, string];

/**
 * A provider profile as returned by the backend (spec section 5).
 *
 * Deliberately contains no API key: the key lives in the OS keyring and is only
 * ever exposed back to the UI as a boolean (see `providersApi.secretStatus`).
 */
export interface ProviderProfile {
  id: string;
  name: string;
  baseUrl: string;
  model: string;
  extraEnv: EnvironmentPair[];
  /**
   * The model's real context window, when the user has declared one. The
   * backend exports it to the session as `CLAUDE_CODE_MAX_CONTEXT_TOKENS` so
   * Claude Code stops sizing auto-compact from an assumed 200k window for a
   * model its shipped catalog does not know (`glm-5.3`, for example).
   *
   * `null` (or absent, for a profile written by an older backend) means "no
   * declared window": Claude Code's own fallback stays in charge.
   */
  maxContextTokens?: number | null;
}

/**
 * Payload for creating or updating a provider profile (no secret).
 *
 * `maxContextTokens` must be forwarded exactly as the form produced it. Omitting
 * it is not the same as leaving it empty: the backend deserializes an absent
 * field to `None` and writes `NULL`, which *clears* a previously declared
 * window. `null` and absent therefore both mean "no declared window" - a
 * dropped field silently erases what the user configured.
 */
export interface ProviderInput {
  name: string;
  baseUrl: string;
  model: string;
  extraEnv: EnvironmentPair[];
  /** Declared context window in tokens; `null`/absent = Claude Code's default. */
  maxContextTokens?: number | null;
}

/** Result of the backend connectivity check (spec section 12). */
export interface ProviderTestResult {
  ok: boolean;
  /** HTTP status when a response arrived; `null` when the request never completed. */
  status: number | null;
  /** Short, secret-free description shown inline in the UI. */
  message: string;
}

/**
 * Whether a provider has an API key stored in the OS keyring.
 * `null` means the status could not be determined (for example the keyring is
 * unavailable) - it never means "the key was read".
 */
export type SecretStatus = boolean | null;

/** Availability of the Rust backend behind `invoke`. */
export type BackendStatus = "connecting" | "ready" | "unavailable" | "error";

/**
 * One agent this build implements, as `list_agents` reports it.
 *
 * Mirrors `crate::agents::AgentDescriptor` (which `app_info.agents` reuses), so
 * the two agent listings can never describe an agent differently. It carries no
 * provider or credential information: this is machine state (is the CLI on
 * `PATH`, where is it), never session state (spec section 17).
 */
export interface AgentInfo {
  /** Stable id persisted in `workspaces.agentId`, e.g. `claude-code`. */
  id: string;
  /** Display label, e.g. `Claude Code`. */
  name: string;
  /**
   * Whether the CLI was found on this machine.
   *
   * `null` means "not determined", and only ever comes from the frontend's own
   * fallback list - the backend always answers with `true` or `false` (the same
   * distinction `SecretStatus` draws). A surface must therefore warn about a
   * missing CLI only on an explicit `false`.
   */
  installed: boolean | null;
  /** Resolved executable path; `null` when the CLI was not found. */
  executablePath: string | null;
}

/**
 * Which palette the user picked (spec section 12, "Appearance").
 *
 * `system` is a real third choice, not "unset": it follows the OS through
 * `prefers-color-scheme` and keeps following it, so a user whose machine flips
 * to light in the evening gets a light app without touching Settings again.
 */
export type ThemeMode = "dark" | "light" | "system";

/**
 * The palette actually painted.
 *
 * `ThemeMode` minus `system`, which is the point of the distinction: only this
 * type reaches the stylesheet and the xterm theme.
 */
export type ResolvedTheme = "dark" | "light";

/**
 * Every stored UI preference, as the backend returns it.
 *
 * Values are **opaque strings** (`get_ui_preference` in the Rust core stores
 * text and never interprets it), so nothing outside `settings/preferences.ts`
 * may read a raw value - that module owns the key names, the defaults and the
 * parsing.
 */
export type UiPreferences = Record<string, string>;

/**
 * Read-only application information for the Settings tab's About section.
 *
 * Mirrors `crate::commands::settings::AppInfo`, which serializes camelCase -
 * the Rust suite pins these exact field names.
 */
export interface AppInfo {
  /** Application version from the bundle metadata. */
  version: string;
  /** Directory holding the database and per-session configuration. */
  dataDirectory: string;
  /** The SQLite database file. */
  databasePath: string;
  /** Database file size in bytes; `null` when the file does not exist yet. */
  databaseSizeBytes: number | null;
  /**
   * Every agent this build implements, with its installation state.
   *
   * The same registry `list_agents` returns, so a consumer uses whichever it
   * already has (this panel loads `app_info` anyway). A list rather than one
   * field per agent, so adding an adapter needs no change here.
   */
  agents: AgentInfo[];
  /** Schema version the database is currently migrated to. */
  schemaVersion: number;
}
