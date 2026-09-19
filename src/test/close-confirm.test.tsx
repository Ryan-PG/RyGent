/**
 * Closing a tab whose agent is still running (spec sections 9, 12).
 *
 * The confirmation is the one behaviour the Settings work *adds* to an existing
 * flow: closing a tab stops its session, and `sessions.confirmCloseRunning`
 * defaults to on. It is rendered by `App`, above `TabBar`, and is deliberately a
 * React modal rather than a native dialog - so it has to be reachable by mouse
 * and keyboard like any other part of the UI, which is what these tests drive.
 *
 * The whole shell is mounted here because that is where the flow lives: the
 * close button is in `TabBar`, the prompt is in `App`, and the decision is in the
 * store. Only the Tauri bridge is faked.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import App from "../App";
import { useAppStore } from "../stores/useAppStore";
import { preferences } from "../settings/preferences";
import {
  makeProvider,
  makeWorkspace,
  resetAppStore,
  seedPreference,
  seedProviders,
  seedSession,
  seedWorkspaces,
  setupUser,
} from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);
vi.mock("@tauri-apps/api/event", async () =>
  (await import("./mock-tauri")).tauriEventModule(),
);

const WORKSPACES = [
  makeWorkspace(),
  makeWorkspace({ id: "ws-2", name: "Beta", projectPath: "D:\\Projects\\beta" }),
];

/** A startable app: one live session in the first tab, nothing else pending. */
function seedRunningApp(): void {
  stubCommands({
    list_ui_preferences: () => ({}),
    list_workspaces: () => WORKSPACES,
    load_workspace_layout: () => ({
      openWorkspaceIds: ["ws-1", "ws-2"],
      activeWorkspaceId: "ws-1",
    }),
    stop_session: () => null,
  });
  seedProviders([makeProvider()]);
  seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
  seedSession("ws-1", { id: "session-1", status: "running" });
}

/** The confirmation prompt, or null when it is not on screen. */
function closePrompt(): HTMLElement | null {
  return screen.queryByRole("dialog", { name: "Close running session" });
}

function closeButton(): HTMLElement {
  return screen.getByRole("button", { name: "Close Alpha" });
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

describe("closing a tab with a running session", () => {
  it("asks before discarding the session, naming the tab it is about", async () => {
    const user = setupUser();
    seedRunningApp();
    render(<App />);

    await user.click(closeButton());

    const prompt = closePrompt();
    expect(prompt).not.toBeNull();
    expect(
      screen.getByRole("heading", { name: "Close Alpha?" }),
    ).toBeInTheDocument();
    // The prompt says what is actually at stake, which is the reason it exists.
    expect(prompt).toHaveTextContent(/still running/i);
    expect(prompt).toHaveTextContent(/stops that session/i);
    // Nothing has happened yet.
    expect(useAppStore.getState().tabs).toHaveLength(2);
    expect(invokeMock).not.toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });

  it("closes the tab and stops the session when the prompt is accepted", async () => {
    const user = setupUser();
    seedRunningApp();
    render(<App />);

    await user.click(closeButton());
    // Scoped to the prompt: the workspace panel offers its own "Close tab".
    const prompt = closePrompt() as HTMLElement;
    await user.click(within(prompt).getByRole("button", { name: "Close tab" }));

    expect(closePrompt()).toBeNull();
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
    // Spec section 9: the workspace stays configured. Only the tab went away.
    expect(useAppStore.getState().workspaces).toHaveLength(2);
  });

  it("keeps the session running when the prompt is dismissed", async () => {
    const user = setupUser();
    seedRunningApp();
    render(<App />);

    await user.click(closeButton());
    await user.click(screen.getByRole("button", { name: "Keep it open" }));

    expect(closePrompt()).toBeNull();
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual([
      "ws-1",
      "ws-2",
    ]);
    expect(useAppStore.getState().sessions["ws-1"]?.status).toBe("running");
    expect(invokeMock).not.toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });

  it("can be dismissed by clicking away from it, like every other modal", async () => {
    const user = setupUser();
    seedRunningApp();
    render(<App />);

    await user.click(closeButton());
    const prompt = closePrompt();
    expect(prompt).not.toBeNull();
    // A click on the backdrop itself - not on the dialog - dismisses.
    fireEvent.mouseDown(prompt?.parentElement as HTMLElement);

    expect(closePrompt()).toBeNull();
    expect(useAppStore.getState().sessions["ws-1"]?.status).toBe("running");
  });

  it("closes without a prompt once the setting is turned off", async () => {
    const user = setupUser();
    seedRunningApp();
    seedPreference(preferences.sessionsConfirmCloseRunning, false);
    render(<App />);

    await user.click(closeButton());

    expect(closePrompt()).toBeNull();
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
  });

  it("does not ask about a tab that is merely open, with no session in it", async () => {
    const user = setupUser();
    seedRunningApp();
    render(<App />);

    // "Beta" was restored as a tab but never started: nothing to lose by closing
    // it, so the default preference must not put a prompt in the way.
    await user.click(screen.getByRole("button", { name: "Close Beta" }));

    expect(closePrompt()).toBeNull();
    expect(useAppStore.getState().tabs.map((tab) => tab.id)).toEqual(["ws-1"]);
  });

  it("is not shown at all when there is no backend to confirm against", () => {
    tauriRuntime.available = false;
    seedWorkspaces(WORKSPACES, ["ws-1"]);

    render(<App />);

    expect(closePrompt()).toBeNull();
  });
});
