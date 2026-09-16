import { create } from "zustand";
import type {
  BackendStatus,
  ProviderInput,
  ProviderProfile,
  ProviderTestResult,
  SecretStatus,
  SessionStatus,
  TerminalSession,
  TerminalSessionStatus,
  Workspace,
  WorkspaceInput,
  WorkspaceTab,
} from "../types";
import { providersApi } from "../services/providers";
import { workspacesApi } from "../services/workspaces";
import { isSessionStatus, sessionsApi } from "../services/sessions";
import {
  errorMessage,
  isBackendAvailable,
  isBackendUnavailableError,
} from "../services/backend";

export type Panel = "workspaces" | "providers" | "settings";

/**
 * Which workspace form is open, if any (spec section 12).
 *
 * Held in the store rather than in a component so the tab bar, the empty state
 * and the workspace panel can all open the same dialog without duplicating it.
 */
export type WorkspaceDialog =
  | { mode: "create" }
  | { mode: "edit"; workspaceId: string }
  | null;

/** The only agent this build implements (spec sections 6, 24). */
export const AGENT_ID = "claude-code";
/** Display label for that agent. */
export const AGENT_LABEL = "Claude Code";

/** Session state for a tab whose agent has never been started. */
const NEW_SESSION: TerminalSession = { status: "created", cols: 80, rows: 24 };

/**
 * Coarse tab status from the session state machine.
 *
 * `starting`/`stopping` map to `running` on purpose: the process is alive (or
 * about to be) and the dot going amber for a fraction of a second before the
 * first output would read as a failure.
 */
export function tabStatusFromSession(status: TerminalSessionStatus): SessionStatus {
  switch (status) {
    case "created":
      return "idle";
    case "starting":
    case "running":
    case "stopping":
      return "running";
    case "stopped":
      return "exited";
    case "failed":
      return "failed";
  }
}

/**
 * A persisted workspace as a shell tab.
 *
 * `id` and `workspaceId` are both the workspace id: one tab per workspace (see
 * `WorkspaceTab` in `types`). `model` falls back to the empty string so the UI
 * can say "provider default" instead of rendering `null`.
 */
function tabFromWorkspace(workspace: Workspace): WorkspaceTab {
  return {
    id: workspace.id,
    workspaceId: workspace.id,
    title: workspace.name,
    projectPath: workspace.projectPath,
    agent: workspace.agentId === AGENT_ID ? AGENT_LABEL : workspace.agentId,
    provider: workspace.providerId,
    model: workspace.model ?? "",
  };
}

/**
 * Re-derive open tabs from a refreshed workspace list.
 *
 * A tab whose workspace no longer exists disappears; the rest pick up renames
 * and provider/model changes. This is the single place tab metadata is refreshed,
 * so no component can show a stale project path.
 */
function syncTabs(
  tabs: WorkspaceTab[],
  workspaces: Workspace[],
): WorkspaceTab[] {
  return tabs
    .map((tab) => workspaces.find((workspace) => workspace.id === tab.id))
    .filter((workspace): workspace is Workspace => workspace !== undefined)
    .map(tabFromWorkspace);
}

/** State changes for closing one tab: drop it, retarget the active tab, forget its session. */
function closedTabState(
  tabs: WorkspaceTab[],
  activeTabId: string | null,
  sessions: Record<string, TerminalSession>,
  id: string,
): { tabs: WorkspaceTab[]; activeTabId: string | null; sessions: Record<string, TerminalSession> } {
  const closedIndex = tabs.findIndex((tab) => tab.id === id);
  const remaining = tabs.filter((tab) => tab.id !== id);
  let nextActive = activeTabId;
  if (activeTabId === id) {
    // Activate the nearest surviving neighbor (right, else left).
    const next = remaining[Math.min(closedIndex, remaining.length - 1)];
    nextActive = next ? next.id : null;
  }
  const remainingSessions = { ...sessions };
  delete remainingSessions[id];
  return { tabs: remaining, activeTabId: nextActive, sessions: remainingSessions };
}

/** Stop a tab's session without waiting, and without surfacing failures. */
function stopSessionQuietly(session: TerminalSession | undefined): void {
  if (session?.id) {
    void sessionsApi.stop(session.id).catch(() => {
      // Closing a tab (or deleting a workspace) while the backend is missing, or
      // after the process already exited, must not produce an error.
    });
  }
}

interface AppState {
  // --- Tab shell (Milestone 1, real data since Milestone 4) -----------------
  /** Open tabs, in the order they were opened. One per workspace. */
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  activePanel: Panel;
  setActiveTab: (id: string) => void;
  setActivePanel: (panel: Panel) => void;
  /**
   * Close a tab. The workspace configuration is **not** deleted (spec section
   * 9) - it stays in `workspaces` and can be reopened from the tab bar's list.
   * The tab's session is stopped, because a tab is that session's only UI.
   */
  closeTab: (id: string) => void;

  // --- Workspace management (Milestone 4) ----------------------------------
  /** Every configured workspace, in creation order. */
  workspaces: Workspace[];
  /** Open/close the New/Edit workspace dialog. */
  workspaceDialog: WorkspaceDialog;
  openWorkspaceDialog: (dialog: WorkspaceDialog) => void;
  /** Set once the first workspace load attempt finished (successfully or not). */
  workspacesLoaded: boolean;
  workspacesLoading: boolean;
  /** List/create/update/delete error, shown as a banner. */
  workspacesError: string | null;
  /**
   * Whether the remembered tab set was read successfully. Until it is true, the
   * store must not write a layout back, or a failed read would overwrite the
   * user's open tabs with nothing.
   */
  layoutRestored: boolean;

  /** Load workspaces and restore the remembered tab set (spec section 14). */
  initializeWorkspaces: () => Promise<void>;
  /** Initialize once (React StrictMode double-invoke safe). */
  ensureWorkspacesLoaded: () => void;
  /** Open a workspace as a tab, or activate its tab if it is already open. */
  openWorkspace: (workspaceId: string) => void;
  /** Create a workspace and open it in a new tab. */
  createWorkspace: (input: WorkspaceInput) => Promise<boolean>;
  /** Rename a workspace or change its folder/agent/provider/model. */
  updateWorkspace: (id: string, input: WorkspaceInput) => Promise<boolean>;
  /** Remove a workspace's configuration and close its tab. */
  deleteWorkspace: (id: string) => Promise<boolean>;
  /** Remember the open tab set (best effort; a UI preference). */
  saveLayout: () => Promise<void>;

  // --- Provider management (Milestone 2) -----------------------------------
  /** Provider profiles loaded from SQLite (metadata only, never keys). */
  providers: ProviderProfile[];
  /** Keyring presence per provider id; `null` = status could not be read. */
  secretStatus: Record<string, SecretStatus>;
  /** Set once the first load attempt finished (successfully or not). */
  providersLoaded: boolean;
  providersLoading: boolean;
  /** List/load-level error, shown as a banner above the list. */
  providersError: string | null;
  /** Availability of the Rust backend. */
  backendStatus: BackendStatus;
  /** Last connectivity-test result per provider id. */
  testResults: Record<string, ProviderTestResult>;
  /** Provider ids with an in-flight connectivity test. */
  testingProviderIds: string[];

  /** Fetch providers + keyring status from the backend. */
  loadProviders: () => Promise<void>;
  /** Load once (React StrictMode double-invoke safe). */
  ensureProvidersLoaded: () => void;
  /** Create or update a provider; writes the key only when one was typed. */
  saveProvider: (
    input: ProviderInput,
    apiKey: string,
    providerId?: string,
  ) => Promise<boolean>;
  /** Delete a provider profile and its keyring entry. */
  deleteProvider: (providerId: string) => Promise<boolean>;
  /** Remove a provider's stored API key (profile stays). */
  clearProviderSecret: (providerId: string) => Promise<boolean>;
  /** Run the backend connectivity check for one provider. */
  testProvider: (providerId: string) => Promise<void>;
  /** Drop the stored test result (used when a provider is edited). */
  clearTestResult: (providerId: string) => void;

  // --- Session runtime (Milestone 3) ---------------------------------------
  /**
   * Per-tab session state, keyed by tab id. Output bytes deliberately do *not*
   * flow through here: they are written straight into the xterm instance by
   * `SessionView`, because a store update per PTY chunk would re-render the
   * whole shell at PTY speed.
   */
  sessions: Record<string, TerminalSession>;
  /** Spawn the agent for a tab; no-op-safe if it is already running. */
  startSession: (tabId: string) => Promise<void>;
  /** Terminate a tab's session and drop its id. */
  stopSession: (tabId: string) => Promise<void>;
  /** Stop + spawn again; starts the session if it never had one. */
  restartSession: (tabId: string) => Promise<void>;
  /** Forward keystrokes from the terminal to the PTY (best effort). */
  writeSessionInput: (tabId: string, data: string) => void;
  /** Record the terminal's fitted size and forward it to the PTY. */
  resizeSession: (tabId: string, cols: number, rows: number) => void;
  /** Apply a lifecycle transition reported by the backend event stream. */
  applySessionState: (
    tabId: string,
    status: TerminalSessionStatus,
    exitCode: number | null,
  ) => void;
}

/** Read the keyring "is a secret set" flag for every provider. */
async function loadSecretStatus(
  providers: ProviderProfile[],
): Promise<{
  secretStatus: Record<string, SecretStatus>;
  errors: string[];
}> {
  const errors: string[] = [];
  const entries = await Promise.all(
    providers.map(async (provider): Promise<[string, SecretStatus]> => {
      try {
        return [provider.id, await providersApi.secretStatus(provider.id)];
      } catch (error) {
        // One unreadable keyring entry must not hide the whole provider list.
        errors.push(errorMessage(error));
        return [provider.id, null];
      }
    }),
  );
  return { secretStatus: Object.fromEntries(entries), errors };
}

/** Immutably patch one tab's session entry, defaulting it when absent. */
function withSession(
  sessions: Record<string, TerminalSession>,
  tabId: string,
  patch: Partial<TerminalSession>,
): Record<string, TerminalSession> {
  return {
    ...sessions,
    [tabId]: { ...(sessions[tabId] ?? NEW_SESSION), ...patch },
  };
}

export const useAppStore = create<AppState>()((set, get) => ({
  // --- Tab shell ------------------------------------------------------------

  // No seeded tabs since M4: every tab is a persisted workspace, loaded from the
  // database on start (spec sections 14, 10). A fresh install shows the empty
  // state and the "New Workspace" dialog.
  tabs: [],
  activeTabId: null,
  activePanel: "workspaces",

  setActiveTab: (id) => set({ activeTabId: id, activePanel: "workspaces" }),

  setActivePanel: (panel) => set({ activePanel: panel }),

  closeTab: (id) => {
    // The PTY keeps running in Rust until it is told to stop, so closing a tab
    // stops its session (spec section 15) while leaving the workspace itself
    // intact (spec section 9).
    stopSessionQuietly(get().sessions[id]);
    set((state) => closedTabState(state.tabs, state.activeTabId, state.sessions, id));
  },

  // --- Workspace management --------------------------------------------------

  workspaces: [],
  workspaceDialog: null,
  workspacesLoaded: false,
  workspacesLoading: false,
  workspacesError: null,
  layoutRestored: false,

  openWorkspaceDialog: (dialog) =>
    set(
      dialog === null
        ? { workspaceDialog: null }
        : // Opening the form clears the previous attempt's error, so the dialog
          // never opens showing a stale message.
          { workspaceDialog: dialog, workspacesError: null },
    ),

  initializeWorkspaces: async () => {
    if (!isBackendAvailable()) {
      // Plain-browser dev: no workspaces, no tabs, and an honest notice. The
      // shell keeps working (spec section 12 is still inspectable).
      set({
        workspaces: [],
        tabs: [],
        activeTabId: null,
        workspacesLoaded: true,
        workspacesLoading: false,
        workspacesError: null,
        layoutRestored: false,
        backendStatus: "unavailable",
      });
      return;
    }

    set({ workspacesLoading: true, workspacesError: null });
    try {
      const [workspaces, layout] = await Promise.all([
        workspacesApi.list(),
        workspacesApi.loadLayout(),
      ]);

      // Reopen the remembered tabs - those whose workspace still exists - and
      // start nothing: sessions come back stopped (spec section 14).
      const openTabs = layout.openWorkspaceIds
        .map((id) => workspaces.find((workspace) => workspace.id === id))
        .filter((workspace): workspace is Workspace => workspace !== undefined)
        .map(tabFromWorkspace);
      const activeTabId =
        openTabs.find((tab) => tab.id === layout.activeWorkspaceId)?.id ??
        openTabs[0]?.id ??
        null;

      set({
        workspaces,
        tabs: openTabs,
        activeTabId,
        workspacesLoaded: true,
        workspacesLoading: false,
        workspacesError: null,
        layoutRestored: true,
        backendStatus: "ready",
      });
    } catch (error) {
      set({
        workspaces: [],
        tabs: [],
        activeTabId: null,
        workspacesLoaded: true,
        workspacesLoading: false,
        // The stored layout was never read, so it must not be overwritten.
        layoutRestored: false,
        backendStatus: isBackendUnavailableError(error) ? "unavailable" : "error",
        workspacesError: errorMessage(error),
      });
    }
  },

  ensureWorkspacesLoaded: () => {
    const { workspacesLoaded, workspacesLoading } = get();
    if (workspacesLoaded || workspacesLoading) {
      return;
    }
    void get().initializeWorkspaces();
  },

  openWorkspace: (workspaceId) =>
    set((state) => {
      const open = state.tabs.find((tab) => tab.id === workspaceId);
      if (open) {
        return { activeTabId: open.id, activePanel: "workspaces" };
      }
      const workspace = state.workspaces.find((entry) => entry.id === workspaceId);
      if (!workspace) {
        // The list is out of date (another window deleted it, or the backend
        // changed under us): report it instead of opening an empty tab.
        return { workspacesError: `workspace not found: ${workspaceId}` };
      }
      return {
        tabs: [...state.tabs, tabFromWorkspace(workspace)],
        activeTabId: workspace.id,
        activePanel: "workspaces",
        workspacesError: null,
      };
    }),

  createWorkspace: async (input) => {
    set({ workspacesError: null });
    try {
      const created = await workspacesApi.create(input);
      const workspaces = await workspacesApi.list();
      set((state) => ({
        workspaces,
        tabs: [...syncTabs(state.tabs, workspaces), tabFromWorkspace(created)],
        activeTabId: created.id,
        activePanel: "workspaces",
        workspaceDialog: null,
      }));
      return true;
    } catch (error) {
      set({ workspacesError: errorMessage(error) });
      return false;
    }
  },

  updateWorkspace: async (id, input) => {
    set({ workspacesError: null });
    try {
      await workspacesApi.update(id, input);
      const workspaces = await workspacesApi.list();
      // Tabs are re-derived, so a rename lands in the tab bar immediately and a
      // changed project folder is visible in the panel before the next start.
      set((state) => ({
        workspaces,
        tabs: syncTabs(state.tabs, workspaces),
        workspaceDialog: null,
      }));
      return true;
    } catch (error) {
      set({ workspacesError: errorMessage(error) });
      return false;
    }
  },

  deleteWorkspace: async (id) => {
    set({ workspacesError: null });
    // Stop the session before the configuration goes away. The backend stops it
    // too (`delete_workspace`); doing it here as well keeps the tab honest if the
    // delete itself fails.
    stopSessionQuietly(get().sessions[id]);
    try {
      await workspacesApi.remove(id);
      const workspaces = await workspacesApi.list();
      set((state) => ({
        workspaces,
        ...closedTabState(state.tabs, state.activeTabId, state.sessions, id),
        workspaceDialog: null,
      }));
      return true;
    } catch (error) {
      set({ workspacesError: errorMessage(error) });
      return false;
    }
  },

  saveLayout: async () => {
    if (!get().layoutRestored) {
      return;
    }
    const { tabs, activeTabId } = get();
    try {
      await workspacesApi.saveLayout({
        openWorkspaceIds: tabs.map((tab) => tab.id),
        activeWorkspaceId: activeTabId,
      });
    } catch {
      // A UI preference is best effort: failing to remember the tab set must
      // never surface as an error or block the shell.
    }
  },

  // --- Provider management ---------------------------------------------------

  providers: [],
  secretStatus: {},
  providersLoaded: false,
  providersLoading: false,
  providersError: null,
  backendStatus: "connecting",
  testResults: {},
  testingProviderIds: [],

  loadProviders: async () => {
    if (!isBackendAvailable()) {
      set({
        providers: [],
        secretStatus: {},
        providersLoaded: true,
        providersLoading: false,
        providersError: null,
        backendStatus: "unavailable",
      });
      return;
    }

    set({ providersLoading: true, providersError: null });
    try {
      const providers = await providersApi.list();
      const { secretStatus, errors } = await loadSecretStatus(providers);
      set({
        providers,
        secretStatus,
        providersLoaded: true,
        providersLoading: false,
        providersError: errors.length > 0 ? errors[0] : null,
        backendStatus: "ready",
      });
    } catch (error) {
      set({
        providersLoaded: true,
        providersLoading: false,
        backendStatus: isBackendUnavailableError(error) ? "unavailable" : "error",
        providersError: errorMessage(error),
      });
    }
  },

  ensureProvidersLoaded: () => {
    const { providersLoaded, providersLoading } = get();
    if (providersLoaded || providersLoading) {
      return;
    }
    void get().loadProviders();
  },

  saveProvider: async (input, apiKey, providerId) => {
    set({ providersError: null });
    try {
      const saved = providerId
        ? await providersApi.update(providerId, input)
        : await providersApi.create(input);

      // Only a newly typed key is written. On edit an empty field means "keep
      // the stored key" - replacing it requires typing a new one (or using the
      // explicit remove action).
      if (apiKey.trim().length > 0) {
        await providersApi.setSecret(saved.id, apiKey);
      }

      get().clearTestResult(saved.id);
      await get().loadProviders();
      return true;
    } catch (error) {
      set({
        providersError: errorMessage(error),
        backendStatus: isBackendUnavailableError(error)
          ? "unavailable"
          : get().backendStatus,
      });
      return false;
    }
  },

  deleteProvider: async (providerId) => {
    set({ providersError: null });
    try {
      await providersApi.remove(providerId);
      set((state) => {
        const testResults = { ...state.testResults };
        delete testResults[providerId];
        return { testResults };
      });
      await get().loadProviders();
      return true;
    } catch (error) {
      set({ providersError: errorMessage(error) });
      return false;
    }
  },

  clearProviderSecret: async (providerId) => {
    set({ providersError: null });
    try {
      // The backend treats an empty key as "delete the stored credential".
      await providersApi.setSecret(providerId, "");
      await get().loadProviders();
      return true;
    } catch (error) {
      set({ providersError: errorMessage(error) });
      return false;
    }
  },

  testProvider: async (providerId) => {
    set((state) => ({
      testingProviderIds: [...state.testingProviderIds, providerId],
    }));
    try {
      const result = await providersApi.test(providerId);
      set((state) => ({
        testResults: { ...state.testResults, [providerId]: result },
      }));
    } catch (error) {
      set((state) => ({
        testResults: {
          ...state.testResults,
          [providerId]: {
            ok: false,
            status: null,
            message: errorMessage(error),
          },
        },
      }));
    } finally {
      set((state) => ({
        testingProviderIds: state.testingProviderIds.filter(
          (id) => id !== providerId,
        ),
      }));
    }
  },

  clearTestResult: (providerId) =>
    set((state) => {
      const testResults = { ...state.testResults };
      delete testResults[providerId];
      return { testResults };
    }),

  // --- Session runtime -------------------------------------------------------

  sessions: {},

  startSession: async (tabId) => {
    const tab = get().tabs.find((t) => t.id === tabId);
    if (!tab) {
      return;
    }
    set((state) => ({
      sessions: withSession(state.sessions, tabId, {
        status: "starting",
        message: undefined,
      }),
    }));
    try {
      // Every tab is a persisted workspace since M4, so this id is a real row
      // and the backend can resolve its project folder, agent and provider.
      const info = await sessionsApi.start(tab.workspaceId);
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          id: info.id,
          status: isSessionStatus(info.status) ? info.status : "running",
          cols: info.cols,
          rows: info.rows,
          message: undefined,
        }),
      }));
    } catch (error) {
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          status: "failed",
          message: errorMessage(error),
        }),
      }));
    }
  },

  stopSession: async (tabId) => {
    const session = get().sessions[tabId];
    if (!session?.id) {
      // Nothing to stop; still record the intent so the chip is honest.
      set((state) => ({
        sessions: withSession(state.sessions, tabId, { status: "stopped" }),
      }));
      return;
    }
    set((state) => ({
      sessions: withSession(state.sessions, tabId, { status: "stopping" }),
    }));
    try {
      await sessionsApi.stop(session.id);
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          id: undefined,
          status: "stopped",
          message: undefined,
        }),
      }));
    } catch (error) {
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          status: "failed",
          message: errorMessage(error),
        }),
      }));
    }
  },

  restartSession: async (tabId) => {
    const session = get().sessions[tabId];
    if (!session?.id) {
      await get().startSession(tabId);
      return;
    }
    set((state) => ({
      sessions: withSession(state.sessions, tabId, {
        status: "starting",
        message: undefined,
      }),
    }));
    try {
      const info = await sessionsApi.restart(session.id);
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          id: info.id,
          status: isSessionStatus(info.status) ? info.status : "running",
          cols: info.cols,
          rows: info.rows,
          message: undefined,
        }),
      }));
    } catch (error) {
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          status: "failed",
          message: errorMessage(error),
        }),
      }));
    }
  },

  writeSessionInput: (tabId, data) => {
    const session = get().sessions[tabId];
    if (!session?.id || (session.status !== "running" && session.status !== "starting")) {
      // Typing into a tab with no live PTY is a no-op, not an error: the
      // terminal is local, so keystrokes still echo before the session starts.
      return;
    }
    void sessionsApi.write(session.id, data).catch((error: unknown) => {
      // A failed write usually means the process died without the state event
      // arriving. Only the first failure updates the store, so a user holding a
      // key down cannot cause a render per keystroke.
      if (get().sessions[tabId]?.status === "failed") {
        return;
      }
      set((state) => ({
        sessions: withSession(state.sessions, tabId, {
          status: "failed",
          message: errorMessage(error),
        }),
      }));
    });
  },

  resizeSession: (tabId, cols, rows) => {
    const session = get().sessions[tabId];
    if (!session || (session.cols === cols && session.rows === rows)) {
      return;
    }
    set((state) => ({
      sessions: withSession(state.sessions, tabId, { cols, rows }),
    }));
    if (session.id) {
      void sessionsApi.resize(session.id, cols, rows).catch(() => {
        // A resize racing a process exit is not worth surfacing.
      });
    }
  },

  applySessionState: (tabId, status, exitCode) =>
    set((state) => ({
      sessions: withSession(state.sessions, tabId, {
        status,
        // The id is only meaningful while the process exists; keeping it after
        // an exit would let a restart target a dead session.
        id: status === "stopped" || status === "failed" ? undefined : state.sessions[tabId]?.id,
        message:
          exitCode === null || exitCode === 0
            ? undefined
            : `agent exited with code ${exitCode}`,
      }),
    })),
}));
