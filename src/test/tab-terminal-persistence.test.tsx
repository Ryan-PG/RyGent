/**
 * Terminals survive a tab switch (spec sections 10, 11).
 *
 * The bug this pins down: `App` used to render only the *active* tab's
 * `WorkspacePanel`. Switching tabs unmounted the old panel and disposed its
 * xterm instance, so returning to the tab showed a brand-new, empty terminal -
 * the PTY kept running in the Rust core, but the earlier output had streamed
 * into a buffer that no longer existed and was never replayed. The tab looked
 * "cleared" while still accepting keystrokes.
 *
 * The fix is to keep every open tab's panel mounted and hide the inactive ones,
 * so the terminal instance (and its scrollback) is preserved and a background
 * tab keeps receiving its output. These tests assert exactly that, through the
 * faked xterm boundary the rest of the terminal suite uses.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import App from "../App";
import { useAppStore } from "../stores/useAppStore";
import { FakeTerminal, resetXtermMock } from "./mock-xterm";
import {
  makeProvider,
  makeWorkspace,
  resetAppStore,
  seedProviders,
  seedSession,
  seedWorkspaces,
  setupUser,
} from "./fixtures";
import {
  emitTauriEvent,
  resetTauriMock,
  stubCommands,
  subscribedChannels,
} from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);
vi.mock("@tauri-apps/api/event", async () =>
  (await import("./mock-tauri")).tauriEventModule(),
);
vi.mock("@xterm/xterm", async () =>
  (await import("./mock-xterm")).xtermModule(),
);
vi.mock("@xterm/addon-fit", async () =>
  (await import("./mock-xterm")).xtermFitAddonModule(),
);

const WORKSPACES = [
  makeWorkspace(),
  makeWorkspace({ id: "ws-2", name: "Beta", projectPath: "D:\\Projects\\beta" }),
];

/** Two open tabs, both with a live session, the first one active. */
function seedTwoLiveTabs(): void {
  stubCommands({
    list_ui_preferences: () => ({}),
    list_workspaces: () => WORKSPACES,
    list_agents: () => [],
    list_providers: () => [makeProvider()],
    provider_secret_status: () => false,
    load_workspace_layout: () => ({
      openWorkspaceIds: ["ws-1", "ws-2"],
      activeWorkspaceId: "ws-1",
    }),
    save_workspace_layout: () => null,
  });
  seedProviders([makeProvider()]);
  seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
  seedSession("ws-1", { id: "session-1", status: "running" });
  seedSession("ws-2", { id: "session-2", status: "running" });
  // `openWorkspace` activates the tab it opens, so Beta ended up active.
  useAppStore.getState().setActiveTab("ws-1");
}

/** The tab-panel wrappers, in tab order. */
function panels(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".tab-panel"));
}

function tab(title: string): HTMLElement {
  return screen.getByRole("tab", { name: new RegExp(title) });
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
  resetXtermMock();
});

describe("terminals across tab switches", () => {
  it("mounts one terminal per open tab, and hides the inactive one", () => {
    seedTwoLiveTabs();
    render(<App />);

    // Both tabs have a terminal; only the active tab's panel is visible.
    expect(FakeTerminal.instances).toHaveLength(2);
    expect(panels()).toHaveLength(2);
    expect(panels()[0].hidden).toBe(false);
    expect(panels()[1].hidden).toBe(true);
  });

  it("does not dispose or rebuild a terminal when switching away and back", async () => {
    const user = setupUser();
    seedTwoLiveTabs();
    render(<App />);

    const [alpha, beta] = FakeTerminal.instances;

    await user.click(tab("Beta"));
    expect(panels()[0].hidden).toBe(true);
    expect(panels()[1].hidden).toBe(false);

    await user.click(tab("Alpha"));
    expect(panels()[0].hidden).toBe(false);
    expect(panels()[1].hidden).toBe(true);

    // The very same instances are still there - nothing was rebuilt.
    expect(FakeTerminal.instances).toHaveLength(2);
    expect(FakeTerminal.instances[0]).toBe(alpha);
    expect(FakeTerminal.instances[1]).toBe(beta);
    expect(alpha.disposed).toBe(false);
    expect(beta.disposed).toBe(false);
  });

  it("keeps writing PTY output into a tab that is in the background", async () => {
    const user = setupUser();
    seedTwoLiveTabs();
    render(<App />);

    await waitFor(() =>
      expect(subscribedChannels()).toContain("session-output:session-1"),
    );
    const alpha = FakeTerminal.instances[0];

    // Alpha is now off-screen; its agent keeps talking.
    await user.click(tab("Beta"));
    act(() => emitTauriEvent("session-output:session-1", "still here\r\n"));

    expect(alpha.writes).toContain("still here\r\n");
  });
});
