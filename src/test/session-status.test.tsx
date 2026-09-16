/**
 * Session status rendering (spec sections 10, 11, 15).
 *
 * The terminal is a real xterm instance, so these tests assert what the UI owns
 * - the lifecycle chip, the toolbar, the status bar and the backend-unavailable
 * notice - and never xterm's canvas/scrollback output. The store is the real
 * store, driven either directly (seeded session state) or through the faked
 * event stream.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import SessionView from "../components/SessionView";
import StatusBar from "../components/StatusBar";
import { tabStatusFromSession, useAppStore } from "../stores/useAppStore";
import type { TerminalSessionStatus } from "../types";
import {
  makeProvider,
  makeWorkspace,
  resetAppStore,
  seedProviders,
  seedSession,
  seedWorkspaces,
  setupUser,
  tabFor,
} from "./fixtures";
import {
  emitTauriEvent,
  invokeMock,
  resetTauriMock,
  stubCommands,
  subscribedChannels,
  tauriRuntime,
} from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);
vi.mock("@tauri-apps/api/event", async () =>
  (await import("./mock-tauri")).tauriEventModule(),
);

const WORKSPACE = makeWorkspace({ model: "model-a" });
const PROVIDER = makeProvider();

/**
 * The lifecycle chip.
 *
 * Scoped by class on purpose: xterm's own screen-reader live region also has
 * the `status` role, so a role query would be ambiguous.
 */
function chip(): HTMLElement {
  const element = document.querySelector(".session-chip");
  if (element === null) {
    throw new Error("no session status chip rendered");
  }
  return element as HTMLElement;
}

/** Open one workspace as a tab and give it a session in `status`. */
function openSession(session: Parameters<typeof seedSession>[1] = {}): void {
  seedWorkspaces([WORKSPACE], ["ws-1"]);
  seedSession("ws-1", session);
}

/** Let the component's async event subscriptions settle. */
async function subscriptionsSettled(): Promise<void> {
  await waitFor(() => expect(subscribedChannels().length).toBeGreaterThan(0));
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

describe("tabStatusFromSession", () => {
  it("maps the session state machine onto the four tab states", () => {
    expect(tabStatusFromSession("created")).toBe("idle");
    // The process is alive (or about to be); amber for a frame would read as a
    // failure, so starting/stopping share "running".
    expect(tabStatusFromSession("starting")).toBe("running");
    expect(tabStatusFromSession("running")).toBe("running");
    expect(tabStatusFromSession("stopping")).toBe("running");
    expect(tabStatusFromSession("stopped")).toBe("exited");
    expect(tabStatusFromSession("failed")).toBe("failed");
  });
});

describe("SessionView: lifecycle indicator", () => {
  const CASES: [TerminalSessionStatus, string][] = [
    ["created", "not started"],
    ["starting", "starting"],
    ["running", "running"],
    ["stopping", "stopping"],
    ["stopped", "stopped"],
    ["failed", "failed"],
  ];

  it.each(CASES)("renders %s as %s", (status, label) => {
    openSession({ status });

    render(<SessionView tab={tabFor("ws-1")} />);

    expect(chip()).toHaveTextContent(label);
    expect(chip()).toHaveClass(`chip-${status}`);
  });

  it("shows the backend's message for a failed session", () => {
    openSession({ status: "failed", message: "spawn failed: claude not found" });

    render(<SessionView tab={tabFor("ws-1")} />);

    expect(chip()).toHaveTextContent("failed");
    expect(
      screen.getByText("spawn failed: claude not found"),
    ).toBeInTheDocument();
  });

  it("renders a tab with no session entry as not started (spec: nothing is started for you)", () => {
    seedWorkspaces([WORKSPACE], ["ws-1"]);
    expect(useAppStore.getState().sessions).toEqual({});

    render(<SessionView tab={tabFor("ws-1")} />);

    expect(chip()).toHaveTextContent("not started");
  });
});

describe("SessionView: no Rust core", () => {
  it("degrades to a visible notice while keeping the view interactive", async () => {
    tauriRuntime.available = false;
    const user = setupUser();
    openSession();

    render(<SessionView tab={tabFor("ws-1")} />);

    // The notice explains how to get a real session...
    expect(screen.getByLabelText("Backend unavailable")).toHaveTextContent(
      /npm run tauri dev/,
    );
    // ...the terminal container is still there (its canvas output is not
    // asserted - jsdom has no canvas)...
    expect(
      screen.getByRole("group", { name: "Agent session terminal" }),
    ).toBeInTheDocument();
    // ...and the toolbar stays live so each button can explain itself.
    expect(screen.getByRole("button", { name: "Start" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Start" }));

    // Nothing was faked: no command was sent and no session was invented.
    expect(invokeMock).not.toHaveBeenCalled();
    expect(useAppStore.getState().sessions["ws-1"].status).toBe("created");
    expect(chip()).toHaveTextContent("not started");
  });
});

describe("SessionView: with the Rust core", () => {
  it("hides the notice and runs the session through the toolbar", async () => {
    const user = setupUser();
    stubCommands({
      start_session: () => ({
        id: "session-1",
        workspaceId: "ws-1",
        status: "running",
        cols: 80,
        rows: 24,
        exitCode: null,
      }),
      stop_session: () => null,
    });
    openSession();

    render(<SessionView tab={tabFor("ws-1")} />);
    expect(screen.queryByLabelText("Backend unavailable")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Start" }));

    await waitFor(() => expect(chip()).toHaveTextContent("running"));
    expect(invokeMock).toHaveBeenCalledWith("start_session", {
      workspaceId: "ws-1",
    });
    // While the process lives, Start is not offered again but Stop is.
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Stop" }));

    await waitFor(() => expect(chip()).toHaveTextContent("stopped"));
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
    expect(useAppStore.getState().sessions["ws-1"].id).toBeUndefined();
  });

  it("follows the backend's lifecycle events and reports a non-zero exit code", async () => {
    openSession({ id: "session-1", status: "running" });

    render(<SessionView tab={tabFor("ws-1")} />);
    await subscriptionsSettled();

    const channel = "session-state:session-1";
    expect(subscribedChannels()).toContain(channel);
    expect(subscribedChannels()).toContain("session-output:session-1");

    // The Rust side may send a bare status string...
    act(() => emitTauriEvent(channel, "stopping"));
    expect(chip()).toHaveTextContent("stopping");

    // ...or a struct, which is also where the exit code arrives.
    act(() => emitTauriEvent(channel, { status: "failed", exitCode: 3 }));
    expect(chip()).toHaveTextContent("failed");
    expect(screen.getByText("agent exited with code 3")).toBeInTheDocument();
  });

  it("reports a spawn failure instead of pretending the session started", async () => {
    const user = setupUser();
    stubCommands({
      start_session: () => {
        throw "claude executable not found on PATH";
      },
    });
    openSession();

    render(<SessionView tab={tabFor("ws-1")} />);
    await user.click(screen.getByRole("button", { name: "Start" }));

    await waitFor(() => expect(chip()).toHaveTextContent("failed"));
    expect(screen.getByText("claude executable not found on PATH")).toBeInTheDocument();
  });
});

describe("StatusBar", () => {
  it("summarises the active tab, its agent and its provider", () => {
    seedWorkspaces([WORKSPACE], ["ws-1"]);
    seedProviders([PROVIDER], { "prov-1": true });
    seedSession("ws-1", { status: "running" });

    render(<StatusBar />);

    expect(
      screen.getByText("running — Alpha · Claude Code · Provider A/model-a"),
    ).toBeInTheDocument();
  });

  it("says provider default when the workspace has no model override", () => {
    seedWorkspaces([makeWorkspace()], ["ws-1"]);
    seedProviders([PROVIDER], { "prov-1": true });

    render(<StatusBar />);

    expect(screen.getByText(/idle — Alpha · Claude Code · Provider A\/provider default/)).toBeInTheDocument();
  });

  it("falls back to the provider id when the profile is gone", () => {
    seedWorkspaces([WORKSPACE], ["ws-1"]);
    seedProviders([], {});

    render(<StatusBar />);

    expect(screen.getByText(/prov-1\/model-a/)).toBeInTheDocument();
  });

  it("says there is no active session when no tab is open", () => {
    render(<StatusBar />);

    expect(screen.getByText("no active session")).toBeInTheDocument();
  });
});
