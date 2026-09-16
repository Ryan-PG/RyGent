/**
 * Store and basic UI state (spec sections 9, 10, 12, 14, 15, 16).
 *
 * These tests drive the store directly: the Tauri *bridge* is faked
 * (`mock-tauri.ts`), everything above it - the service wrappers, error
 * normalization and the store's reducers - is the real code.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "../stores/useAppStore";
import { makeWorkspace, resetAppStore, seedSession, seedWorkspaces } from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

const WORKSPACES = [
  makeWorkspace(),
  makeWorkspace({ id: "ws-2", name: "Beta", projectPath: "D:\\Projects\\beta" }),
  makeWorkspace({ id: "ws-3", name: "Gamma", projectPath: "D:\\Projects\\gamma" }),
];

/** Stubs every command a successful app start issues. */
function stubStartup(
  workspaces = WORKSPACES,
  layout: { openWorkspaceIds: string[]; activeWorkspaceId: string | null } = {
    openWorkspaceIds: [],
    activeWorkspaceId: null,
  },
): void {
  stubCommands({
    list_workspaces: () => workspaces,
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

  it("loads only once, so StrictMode's double invoke cannot refetch", () => {
    stubStartup();

    useAppStore.getState().ensureWorkspacesLoaded();
    useAppStore.getState().ensureWorkspacesLoaded();

    const listCalls = invokeMock.mock.calls.filter(
      ([command]) => command === "list_workspaces",
    );
    expect(listCalls).toHaveLength(1);
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
