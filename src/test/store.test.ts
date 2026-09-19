/**
 * Store and basic UI state (spec sections 9, 10, 12, 14, 15, 16).
 *
 * These tests drive the store directly: the Tauri *bridge* is faked
 * (`mock-tauri.ts`), everything above it - the service wrappers, error
 * normalization and the store's reducers - is the real code.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "../stores/useAppStore";
import { preferences } from "../settings/preferences";
import { resolveTheme } from "../settings/theme";
import {
  makeAgents,
  makeWorkspace,
  resetAppStore,
  seedAgents,
  seedPreference,
  seedPreferences,
  seedSession,
  seedWorkspaces,
} from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

const WORKSPACES = [
  makeWorkspace(),
  makeWorkspace({ id: "ws-2", name: "Beta", projectPath: "D:\\Projects\\beta" }),
  makeWorkspace({ id: "ws-3", name: "Gamma", projectPath: "D:\\Projects\\gamma" }),
];

/** The same kind of workspace, running the other agent this build ships. */
const CODEX_WORKSPACE = makeWorkspace({
  id: "ws-codex",
  name: "Delta",
  projectPath: "D:\\Projects\\delta",
  agentId: "codex",
});

/**
 * Stubs every command a successful app start issues.
 *
 * `list_ui_preferences` belongs here because `initializeWorkspaces` awaits the
 * preferences first - the restore-tabs preference decides whether the stored
 * layout is used at all. `list_agents` is read by the same startup path, because
 * it is what turns a workspace's `agentId` into the CLI's name.
 */
function stubStartup(
  workspaces = WORKSPACES,
  layout: { openWorkspaceIds: string[]; activeWorkspaceId: string | null } = {
    openWorkspaceIds: [],
    activeWorkspaceId: null,
  },
): void {
  stubCommands({
    list_ui_preferences: () => ({}),
    list_workspaces: () => workspaces,
    list_agents: () => makeAgents(),
    load_workspace_layout: () => layout,
    save_workspace_layout: (args) => args?.layout,
  });
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

describe("app store: initial state", () => {
  it("boots with an empty shell and nothing loaded", () => {
    const state = useAppStore.getState();

    expect(state.tabs).toEqual([]);
    expect(state.activeTabId).toBeNull();
    expect(state.activePanel).toBe("workspaces");

    expect(state.workspaces).toEqual([]);
    expect(state.workspacesLoaded).toBe(false);
    expect(state.workspacesError).toBeNull();
    expect(state.layoutRestored).toBe(false);
    expect(state.workspaceDialog).toBeNull();

    expect(state.providers).toEqual([]);
    expect(state.secretStatus).toEqual({});
    expect(state.providersLoaded).toBe(false);
    expect(state.providersError).toBeNull();
    expect(state.backendStatus).toBe("connecting");

    expect(state.sessions).toEqual({});

    expect(state.agents).toEqual([]);
    expect(state.agentsLoaded).toBe(false);
  });
});

describe("app store: loading workspaces", () => {
  it("populates the tabs from the remembered layout", async () => {
    stubStartup(WORKSPACES, {
      openWorkspaceIds: ["ws-2", "ws-1"],
      activeWorkspaceId: "ws-1",
    });

    await useAppStore.getState().initializeWorkspaces();

    const state = useAppStore.getState();
    expect(state.workspaces.map((workspace) => workspace.id)).toEqual([
      "ws-1",
      "ws-2",
      "ws-3",
    ]);
    // Tabs follow the stored order, not the workspace order.
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-2", "ws-1"]);
    expect(state.activeTabId).toBe("ws-1");
    expect(state.workspacesLoaded).toBe(true);
    expect(state.layoutRestored).toBe(true);
    expect(state.backendStatus).toBe("ready");

    // Restoring a tab opens a view, never a process (spec section 14).
    expect(state.sessions).toEqual({});
    expect(invokeMock).not.toHaveBeenCalledWith(
      "start_session",
      expect.anything(),
    );
  });

  it("builds each tab from its workspace, and falls back to the first tab when the remembered active one is gone", async () => {
    stubStartup(WORKSPACES, {
      openWorkspaceIds: ["ws-1", "ws-gone"],
      activeWorkspaceId: "ws-gone",
    });

    await useAppStore.getState().initializeWorkspaces();

    const state = useAppStore.getState();
    // A layout entry whose workspace was deleted is dropped, not rendered.
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-1"]);
    expect(state.activeTabId).toBe("ws-1");
    expect(state.tabs[0]).toMatchObject({
      title: "Alpha",
      projectPath: "D:\\Projects\\alpha",
      agent: "Claude Code",
      provider: "prov-1",
      model: "",
    });
  });

  it("loads only once, so StrictMode's double invoke cannot refetch", async () => {
    stubStartup();

    // Both calls are made before either finishes, which is the whole point: the
    // second is turned away by the in-flight guard, not by winning a race.
    useAppStore.getState().ensureWorkspacesLoaded();
    useAppStore.getState().ensureWorkspacesLoaded();

    // `workspacesLoading` is set before the first `await`, so the guard itself is
    // settled here - but the fetch is now issued several awaits in (preferences
    // are read first), so the call count is not observable until they drain.
    await vi.waitFor(() => {
      const listCalls = invokeMock.mock.calls.filter(
        ([command]) => command === "list_workspaces",
      );
      expect(listCalls).toHaveLength(1);
    });
    // Preferences were fetched once by the same guard, not once per caller.
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "list_ui_preferences",
      ),
    ).toHaveLength(1);
  });

  it("reports a plain-browser start as a notice, not as an error", async () => {
    tauriRuntime.available = false;

    await expect(
      useAppStore.getState().initializeWorkspaces(),
    ).resolves.toBeUndefined();

    const state = useAppStore.getState();
    expect(invokeMock).not.toHaveBeenCalled();
    expect(state.backendStatus).toBe("unavailable");
    expect(state.workspaces).toEqual([]);
    expect(state.tabs).toEqual([]);
    expect(state.activeTabId).toBeNull();
    // Loaded-and-empty, so the shell shows the empty state rather than a spinner.
    expect(state.workspacesLoaded).toBe(true);
    expect(state.workspacesLoading).toBe(false);
    // A missing backend is a notice; an error banner would imply a failed call.
    expect(state.workspacesError).toBeNull();
    // The stored layout was never read, so it must not be written back.
    expect(state.layoutRestored).toBe(false);
    // No agent list either - and marked loaded, so no retry loop starts.
    expect(state.agents).toEqual([]);
    expect(state.agentsLoaded).toBe(true);

    await useAppStore.getState().loadProviders();
    expect(useAppStore.getState().backendStatus).toBe("unavailable");
    expect(useAppStore.getState().providersLoaded).toBe(true);
    expect(useAppStore.getState().providersError).toBeNull();
  });

  it("does not remember a layout it could not read", async () => {
    tauriRuntime.available = false;
    await useAppStore.getState().initializeWorkspaces();

    await useAppStore.getState().saveLayout();

    expect(invokeMock).not.toHaveBeenCalledWith(
      "save_workspace_layout",
      expect.anything(),
    );
  });

  it("surfaces a backend failure as a message (Rust rejects with a plain string)", async () => {
    stubCommands({
      // Stubbed so the only failure in play is the one under test.
      list_ui_preferences: () => ({}),
      list_workspaces: () => {
        throw "database is locked";
      },
      load_workspace_layout: () => ({
        openWorkspaceIds: [],
        activeWorkspaceId: null,
      }),
    });

    await useAppStore.getState().initializeWorkspaces();

    const state = useAppStore.getState();
    expect(state.workspacesError).toBe("database is locked");
    expect(state.backendStatus).toBe("error");
    expect(state.workspacesLoaded).toBe(true);
    expect(state.tabs).toEqual([]);
  });
});

describe("app store: tab open/close semantics", () => {
  it("closes a tab without deleting the workspace, and reopening restores it", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2", "ws-3"]);

    useAppStore.getState().closeTab("ws-2");

    const closed = useAppStore.getState();
    expect(closed.tabs.map((tab) => tab.id)).toEqual(["ws-1", "ws-3"]);
    // Spec section 9: closing a tab is not removing a workspace.
    expect(closed.workspaces.map((workspace) => workspace.id)).toEqual([
      "ws-1",
      "ws-2",
      "ws-3",
    ]);

    useAppStore.getState().openWorkspace("ws-2");

    const reopened = useAppStore.getState();
    expect(reopened.tabs.map((tab) => tab.id)).toEqual([
      "ws-1",
      "ws-3",
      "ws-2",
    ]);
    expect(reopened.activeTabId).toBe("ws-2");
    expect(reopened.tabs[2]).toMatchObject({
      title: "Beta",
      projectPath: "D:\\Projects\\beta",
      workspaceId: "ws-2",
    });
  });

  it("closing the active tab activates the nearest survivor, and the last close leaves no active tab", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2", "ws-3"]);
    useAppStore.getState().setActiveTab("ws-2");

    useAppStore.getState().closeTab("ws-2");
    expect(useAppStore.getState().activeTabId).toBe("ws-3");

    useAppStore.getState().closeTab("ws-3");
    useAppStore.getState().closeTab("ws-1");

    const state = useAppStore.getState();
    expect(state.tabs).toEqual([]);
    expect(state.activeTabId).toBeNull();
    // Still configured: the user can reopen any of them.
    expect(state.workspaces).toHaveLength(3);
  });

  it("closing a background tab leaves the active tab alone", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    useAppStore.getState().setActiveTab("ws-2");

    useAppStore.getState().closeTab("ws-1");

    expect(useAppStore.getState().activeTabId).toBe("ws-2");
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
  });

  it("stops only the closed tab's session, and forgets its state", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });
    seedSession("ws-2", { id: "session-2", status: "running" });
    const otherSession = useAppStore.getState().sessions["ws-2"];

    useAppStore.getState().closeTab("ws-1");

    // A tab is its session's only UI, so closing it stops the process...
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
    expect(invokeMock).not.toHaveBeenCalledWith("stop_session", {
      sessionId: "session-2",
    });
    // ...and drops the session entry, while the other tab keeps running.
    const sessions = useAppStore.getState().sessions;
    expect(sessions["ws-1"]).toBeUndefined();
    expect(sessions["ws-2"]).toEqual(otherSession);
  });

  it("switching the active tab does not touch any tab's session", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    const { applySessionState, setActiveTab } = useAppStore.getState();
    applySessionState("ws-1", "running", null);
    applySessionState("ws-2", "stopped", 0);
    const sessionsBefore = useAppStore.getState().sessions;
    const tabsBefore = useAppStore.getState().tabs;

    setActiveTab("ws-2");

    const state = useAppStore.getState();
    expect(state.activeTabId).toBe("ws-2");
    // Same objects: switching tabs only changes which session is visible
    // (spec section 10), it never terminates or rewrites another one.
    expect(state.sessions).toEqual(sessionsBefore);
    expect(state.sessions["ws-1"]).toBe(sessionsBefore["ws-1"]);
    expect(state.tabs).toBe(tabsBefore);
  });

  it("reopening an already-open tab just activates it", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    useAppStore.getState().setActiveTab("ws-1");

    useAppStore.getState().openWorkspace("ws-2");

    expect(useAppStore.getState().tabs).toHaveLength(2);
    expect(useAppStore.getState().activeTabId).toBe("ws-2");
  });

  it("reports a workspace that vanished from the list instead of opening an empty tab", () => {
    seedWorkspaces(WORKSPACES, []);

    useAppStore.getState().openWorkspace("ws-deleted");

    const state = useAppStore.getState();
    expect(state.tabs).toEqual([]);
    expect(state.workspacesError).toBe("workspace not found: ws-deleted");
  });
});

describe("app store: workspace lifecycle", () => {
  it("creates a workspace, opens it in a tab and closes the dialog", async () => {
    const created = makeWorkspace({
      id: "ws-new",
      name: "Delta",
      projectPath: "D:\\Projects\\delta",
    });
    stubCommands({
      create_workspace: () => created,
      list_workspaces: () => [...WORKSPACES, created],
    });
    useAppStore.setState({ workspaceDialog: { mode: "create" } });

    const ok = await useAppStore.getState().createWorkspace({
      name: "Delta",
      projectPath: "D:\\Projects\\delta",
      agentId: "claude-code",
      providerId: "prov-1",
      model: null,
    });

    expect(ok).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("create_workspace", {
      input: {
        name: "Delta",
        projectPath: "D:\\Projects\\delta",
        agentId: "claude-code",
        providerId: "prov-1",
        model: null,
      },
    });
    const state = useAppStore.getState();
    expect(state.workspaceDialog).toBeNull();
    expect(state.activeTabId).toBe("ws-new");
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-new"]);
    expect(state.workspacesError).toBeNull();
  });

  it("keeps the dialog open and reports the reason when creation fails", async () => {
    stubCommands({
      create_workspace: () => {
        throw "project folder does not exist: D:\\nope";
      },
    });
    useAppStore.setState({ workspaceDialog: { mode: "create" } });

    const ok = await useAppStore.getState().createWorkspace({
      name: "Delta",
      projectPath: "D:\\nope",
      agentId: "claude-code",
      providerId: "prov-1",
      model: null,
    });

    expect(ok).toBe(false);
    const state = useAppStore.getState();
    expect(state.workspacesError).toBe(
      "project folder does not exist: D:\\nope",
    );
    expect(state.workspaceDialog).toEqual({ mode: "create" });
    expect(state.tabs).toEqual([]);
  });

  it("re-derives open tabs after a rename, and deletes a workspace with its tab", async () => {
    const renamed = { ...WORKSPACES[0], name: "Alpha renamed" };
    // A mutable row store, so a delete is visible to the next list call.
    let rows = [renamed, WORKSPACES[1]];
    stubCommands({
      update_workspace: () => renamed,
      list_workspaces: () => rows,
      delete_workspace: () => {
        rows = [WORKSPACES[1]];
        return null;
      },
      stop_session: () => null,
    });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);

    await useAppStore.getState().updateWorkspace("ws-1", {
      name: "Alpha renamed",
      projectPath: "D:\\Projects\\alpha",
      agentId: "claude-code",
      providerId: "prov-1",
      model: null,
    });
    expect(useAppStore.getState().tabs[0].title).toBe("Alpha renamed");

    const deleted = await useAppStore.getState().deleteWorkspace("ws-1");

    expect(deleted).toBe(true);
    const state = useAppStore.getState();
    expect(state.workspaces.map((workspace) => workspace.id)).toEqual(["ws-2"]);
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
  });
});

describe("app store: agents", () => {
  it("names a tab's agent from the backend list rather than from a hardcoded label", async () => {
    stubStartup([CODEX_WORKSPACE, WORKSPACES[0]], {
      openWorkspaceIds: ["ws-codex", "ws-1"],
      activeWorkspaceId: "ws-codex",
    });

    await useAppStore.getState().initializeWorkspaces();

    // Both agents coexist: two workspaces, two tabs, each labelled by its own
    // CLI. Nothing about the tab machinery is agent-specific.
    expect(useAppStore.getState().tabs.map((tab) => tab.agent)).toEqual([
      "Codex",
      "Claude Code",
    ]);
  });

  it("falls back to the built-in names when the list cannot be read, and says nothing about it", async () => {
    stubCommands({
      list_ui_preferences: () => ({}),
      list_workspaces: () => [CODEX_WORKSPACE],
      list_agents: () => {
        throw "agent discovery is unavailable";
      },
      load_workspace_layout: () => ({
        openWorkspaceIds: ["ws-codex"],
        activeWorkspaceId: "ws-codex",
      }),
    });

    await useAppStore.getState().initializeWorkspaces();

    const state = useAppStore.getState();
    // The built-in list still names the agent, so the tab is describable...
    expect(state.tabs[0].agent).toBe("Codex");
    // ...and a cosmetic failure is not reported as a blocking one.
    expect(state.workspacesError).toBeNull();
    expect(state.agents).toEqual([]);
    expect(state.agentsLoaded).toBe(true);
  });

  it("loads the list once, so StrictMode's double invoke cannot refetch", async () => {
    stubCommands({ list_agents: () => makeAgents() });

    useAppStore.getState().ensureAgentsLoaded();
    useAppStore.getState().ensureAgentsLoaded();

    await vi.waitFor(() => {
      expect(useAppStore.getState().agents).toHaveLength(2);
    });
    expect(
      invokeMock.mock.calls.filter(([command]) => command === "list_agents"),
    ).toHaveLength(1);
  });

  it("re-derives a tab's agent when the workspace is switched to another one", async () => {
    const switched = { ...CODEX_WORKSPACE, id: "ws-1", name: "Alpha" };
    stubCommands({
      update_workspace: () => switched,
      list_workspaces: () => [switched],
    });
    seedAgents();
    seedWorkspaces([WORKSPACES[0]], ["ws-1"]);

    await useAppStore.getState().updateWorkspace("ws-1", {
      name: "Alpha",
      projectPath: "D:\\Projects\\alpha",
      agentId: "codex",
      providerId: "prov-1",
      model: null,
    });

    // The panel says which CLI the tab runs before the next start, not after it.
    expect(useAppStore.getState().tabs[0].agent).toBe("Codex");
  });

  it("keeps an unknown agent id visible instead of hiding the workspace", () => {
    seedAgents();
    seedWorkspaces(
      [makeWorkspace({ id: "ws-future", name: "Future", agentId: "gemini" })],
      ["ws-future"],
    );

    // A workspace written by a build that knew another adapter must still be
    // describable: the id is shown as itself rather than blanked out.
    expect(useAppStore.getState().tabs[0].agent).toBe("gemini");
  });
});

describe("app store: preferences", () => {
  afterEach(() => {
    // The theme is applied to the real document, which outlives the test.
    delete document.documentElement.dataset.theme;
    localStorage.clear();
  });

  it("loads the stored map once, and resolves the theme from it", async () => {
    stubCommands({
      list_ui_preferences: () => ({ "appearance.theme": "light" }),
    });

    await useAppStore.getState().initializePreferences();

    const state = useAppStore.getState();
    expect(state.preferences).toEqual({ "appearance.theme": "light" });
    expect(state.preferencesLoaded).toBe(true);
    expect(state.preferencesError).toBeNull();
    expect(state.resolvedTheme).toBe("light");
    // A theme change is also a repaint of the document, not just a store value.
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("follows the OS while the stored mode is system", async () => {
    // The harness answers "no" to every media query, so system means light.
    stubCommands({ list_ui_preferences: () => ({}) });

    await useAppStore.getState().initializePreferences();

    expect(useAppStore.getState().resolvedTheme).toBe(
      resolveTheme("system", false),
    );
  });

  it("only loads once, so StrictMode's double invoke cannot refetch", async () => {
    stubCommands({ list_ui_preferences: () => ({}) });

    useAppStore.getState().ensurePreferencesLoaded();
    useAppStore.getState().ensurePreferencesLoaded();

    await vi.waitFor(() => {
      expect(
        invokeMock.mock.calls.filter(
          ([command]) => command === "list_ui_preferences",
        ),
      ).toHaveLength(1);
    });
  });

  it("falls back to the defaults and reports the failure when the read fails", async () => {
    stubCommands({
      list_ui_preferences: () => {
        throw "database is locked";
      },
    });

    await useAppStore.getState().initializePreferences();

    const state = useAppStore.getState();
    // Loaded-and-empty, so the panel shows the defaults rather than a spinner.
    expect(state.preferences).toEqual({});
    expect(state.preferencesLoaded).toBe(true);
    expect(state.preferencesError).toBe("database is locked");
    expect(state.resolvedTheme).toBe(resolveTheme("system", false));
  });

  it("writes a preference, updating the UI before the backend answers", async () => {
    stubCommands({ set_ui_preference: () => null });

    const ok = await useAppStore
      .getState()
      .setPreference("terminal.fontSize", "18");

    expect(ok).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("set_ui_preference", {
      key: "terminal.fontSize",
      value: "18",
    });
    expect(useAppStore.getState().preferences["terminal.fontSize"]).toBe("18");
  });

  it("resets by writing the empty value, and drops the key locally", async () => {
    stubCommands({ set_ui_preference: () => null });
    seedPreferences({ "terminal.fontSize": "18" });

    await useAppStore.getState().resetPreference("terminal.fontSize");

    // An empty value is how the backend deletes the row, so "unset" and "set to
    // empty" stay the same thing on both sides (spec section 14).
    expect(invokeMock).toHaveBeenCalledWith("set_ui_preference", {
      key: "terminal.fontSize",
      value: "",
    });
    expect(useAppStore.getState().preferences).toEqual({});
    expect(preferences.terminalFontSize.get(useAppStore.getState().preferences)).toBe(
      12,
    );
  });

  it("rolls a failed write back and reports why", async () => {
    stubCommands({
      set_ui_preference: () => {
        throw "database is locked";
      },
    });
    seedPreferences({ "terminal.fontSize": "18" });

    const ok = await useAppStore
      .getState()
      .setPreference("terminal.fontSize", "20");

    expect(ok).toBe(false);
    const state = useAppStore.getState();
    // A control must never show a value that is not stored.
    expect(state.preferences["terminal.fontSize"]).toBe("18");
    expect(state.preferencesError).toBe("database is locked");
  });

  it("keeps a change in memory without persisting it when there is no backend", async () => {
    tauriRuntime.available = false;

    const ok = await useAppStore
      .getState()
      .setPreference("appearance.theme", "dark");

    // Nothing persists in a plain browser, but the session behaves as if it did,
    // so the panel stays usable for inspecting the layout.
    expect(ok).toBe(true);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(useAppStore.getState().resolvedTheme).toBe("dark");
  });
});

describe("app store: the restore-tabs preference", () => {
  it("reopens the remembered tabs by default", async () => {
    stubStartup(WORKSPACES, {
      openWorkspaceIds: ["ws-1", "ws-2"],
      activeWorkspaceId: "ws-1",
    });

    await useAppStore.getState().initializeWorkspaces();

    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual([
      "ws-1",
      "ws-2",
    ]);
  });

  it("starts on an empty shell when the user turned restoring off", async () => {
    stubStartup(WORKSPACES, {
      openWorkspaceIds: ["ws-1", "ws-2"],
      activeWorkspaceId: "ws-1",
    });
    seedPreference(preferences.sessionsRestoreTabs, false);

    await useAppStore.getState().initializeWorkspaces();

    const state = useAppStore.getState();
    expect(state.tabs).toEqual([]);
    expect(state.activeTabId).toBeNull();
    // The workspaces themselves are always loaded - the sidebar is not a tab.
    expect(state.workspaces.map((workspace) => workspace.id)).toEqual([
      "ws-1",
      "ws-2",
      "ws-3",
    ]);
    // The stored layout *was* read, so the tab set may keep being remembered:
    // with an empty layout stored, turning the setting back on would otherwise
    // restore nothing at all.
    expect(state.layoutRestored).toBe(true);
  });
});

describe("app store: closing a tab whose session is running", () => {
  it("closes straight away when nothing is running in that tab", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "created" });

    useAppStore.getState().requestCloseTab("ws-1");

    expect(useAppStore.getState().confirmCloseTabId).toBeNull();
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
  });

  it("parks the close and asks first when the agent is live", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });

    useAppStore.getState().requestCloseTab("ws-1");

    const state = useAppStore.getState();
    expect(state.confirmCloseTabId).toBe("ws-1");
    // Nothing has happened yet: the session is untouched and the tab is open.
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-1", "ws-2"]);
    expect(invokeMock).not.toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });

  it("closes the tab, and stops its session, once the prompt is confirmed", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });

    useAppStore.getState().requestCloseTab("ws-1");
    useAppStore.getState().confirmCloseTab();

    const state = useAppStore.getState();
    expect(state.confirmCloseTabId).toBeNull();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });

  it("leaves everything alone when the prompt is dismissed", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });

    useAppStore.getState().requestCloseTab("ws-1");
    useAppStore.getState().cancelCloseTab();

    const state = useAppStore.getState();
    expect(state.confirmCloseTabId).toBeNull();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-1", "ws-2"]);
    expect(state.sessions["ws-1"]?.id).toBe("session-1");
  });

  it("asks about nothing once the tab is closed some other way", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });

    useAppStore.getState().requestCloseTab("ws-1");
    // A delete in another window, say. The prompt must not outlive its tab.
    useAppStore.getState().closeTab("ws-1");

    expect(useAppStore.getState().confirmCloseTabId).toBeNull();
  });

  it("closes a running session without asking when the preference is off", () => {
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });
    seedPreference(preferences.sessionsConfirmCloseRunning, false);

    useAppStore.getState().requestCloseTab("ws-1");

    const state = useAppStore.getState();
    expect(state.confirmCloseTabId).toBeNull();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });
});
