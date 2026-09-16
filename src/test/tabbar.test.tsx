/**
 * Tab management UI (spec sections 9, 10, 12).
 *
 * `TabBar` is driven through real clicks (`user-event`); the store is the real
 * store, with only the Tauri bridge faked.
 *
 * The reopen list is portaled to `<body>`, so most of these tests query it
 * through `screen` (which searches the whole document) rather than through the
 * render container. jsdom has no layout engine and loads no stylesheet, so it
 * cannot observe the *clip* that hid this menu in the running app: what it can
 * observe is where the popover is mounted and what coordinates it is given,
 * which is what the placement tests below supply by hand.
 */

import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import TabBar from "../components/TabBar";
import WorkspaceSwitcher from "../components/WorkspaceSwitcher";
import { useAppStore } from "../stores/useAppStore";
import {
  makeWorkspace,
  resetAppStore,
  seedSession,
  seedWorkspaces,
  setupUser,
} from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

const WORKSPACES = [
  makeWorkspace(),
  makeWorkspace({ id: "ws-2", name: "Beta", projectPath: "D:\\Projects\\beta" }),
];

/**
 * Three configured workspaces - the shape of the reported bug (three configured
 * workspaces, none of them openable).
 */
const GAMMA = makeWorkspace({
  id: "ws-3",
  name: "Gamma",
  projectPath: "D:\\Projects\\gamma",
});
const THREE_WORKSPACES = [...WORKSPACES, GAMMA];

/**
 * The rendered tab whose title contains `name`.
 *
 * A tab's accessible name is its content - status indicator, title and close
 * button ("idleAlpha ×") - so a plain string would have to match all of it.
 */
function tab(name: string | RegExp): HTMLElement {
  return screen.getByRole("tab", {
    name: typeof name === "string" ? new RegExp(name) : name,
  });
}

/** The tab bar's `Open ▾` trigger. */
function openButton(): HTMLElement {
  return screen.getByRole("button", { name: "Open ▾" });
}

/** The reopen list, which lives in a body-level portal. */
function switcherMenu(): HTMLElement {
  return screen.getByRole("menu", { name: "Configured workspaces" });
}

/** The rendered menu entry whose text contains `name`. */
function menuItem(name: string | RegExp): HTMLElement {
  return screen.getByRole("menuitem", {
    name: typeof name === "string" ? new RegExp(name) : name,
  });
}

/**
 * Render the list on its own. The anchor ref is null here - there is no tab bar
 * to hang it from - which parks the menu at the window's top-left corner
 * instead of under a button; `TabBar`'s tests cover the real geometry.
 */
function renderSwitcher(onClose: () => void = vi.fn()) {
  const anchorRef: RefObject<HTMLElement | null> = { current: null };
  return {
    onClose,
    ...render(<WorkspaceSwitcher anchorRef={anchorRef} onClose={onClose} />),
  };
}

/** A `DOMRect` built from the parts a test cares about. */
function rect(parts: Partial<DOMRect>): DOMRect {
  const left = parts.left ?? 0;
  const top = parts.top ?? 0;
  const right = parts.right ?? left + (parts.width ?? 0);
  const bottom = parts.bottom ?? top + (parts.height ?? 0);
  return {
    left,
    top,
    right,
    bottom,
    width: right - left,
    height: bottom - top,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

/**
 * Give the trigger and the menu a size, because jsdom's layout engine does not
 * exist: every rect it reports is 0×0 at the window's origin, so the geometry
 * the placement reads has to come from the test. Everything else keeps jsdom's
 * zero rect.
 */
function stubSwitcherRects(
  anchor: Partial<DOMRect>,
  menu: Partial<DOMRect>,
): void {
  const anchorRect = rect(anchor);
  const menuRect = rect(menu);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.classList.contains("workspace-switcher")) {
        return menuRect;
      }
      if (this.classList.contains("open-workspace")) {
        return anchorRect;
      }
      return rect({});
    },
  );
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

// Undo the rect stub; the Tauri bridge is re-installed by `resetTauriMock`.
afterEach(() => {
  vi.restoreAllMocks();
});

describe("TabBar", () => {
  it("renders one tab per open workspace and marks the active one", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    useAppStore.getState().setActiveTab("ws-2");

    render(<TabBar />);

    const tabs = screen.getAllByRole("tab");
    expect(tabs).toHaveLength(2);
    expect(within(tabs[0]).getByText("Alpha")).toBeInTheDocument();
    expect(within(tabs[1]).getByText("Beta")).toBeInTheDocument();

    expect(tab("Beta")).toHaveAttribute("aria-selected", "true");
    expect(tab("Alpha")).toHaveAttribute("aria-selected", "false");
  });

  it("clicking a tab makes it the active one", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    render(<TabBar />);

    await user.click(tab("Alpha"));

    expect(useAppStore.getState().activeTabId).toBe("ws-1");
    expect(tab("Alpha")).toHaveAttribute("aria-selected", "true");
  });

  it("the close button removes only that tab and keeps the workspace configured", async () => {
    const user = setupUser();
    stubCommands({ stop_session: () => null });
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { id: "session-1", status: "running" });

    render(<TabBar />);
    await user.click(screen.getByRole("button", { name: "Close Alpha" }));

    const state = useAppStore.getState();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-2"]);
    // Spec section 9: the configuration survives closing the tab.
    expect(state.workspaces.map((workspace) => workspace.id)).toEqual([
      "ws-1",
      "ws-2",
    ]);
    expect(invokeMock).toHaveBeenCalledWith("stop_session", {
      sessionId: "session-1",
    });
    expect(screen.queryByRole("tab", { name: /Alpha/ })).not.toBeInTheDocument();
    expect(tab("Beta")).toBeInTheDocument();
  });

  it("shows each tab's session status on its indicator", () => {
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    seedSession("ws-1", { status: "running" });
    seedSession("ws-2", { status: "failed" });

    render(<TabBar />);

    // A tab whose agent was never started has no session entry and reads idle.
    expect(within(tab("Alpha")).getByLabelText("running")).toHaveClass(
      "dot-running",
    );
    expect(within(tab("Beta")).getByLabelText("failed")).toHaveClass(
      "dot-failed",
    );
  });

  it("still reads idle for a tab with no session yet", () => {
    seedWorkspaces(WORKSPACES, ["ws-1"]);
    render(<TabBar />);
    expect(within(tab("Alpha")).getByLabelText("idle")).toHaveClass("dot-idle");
  });

  it("the new-workspace action opens the create dialog", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, []);
    render(<TabBar />);

    await user.click(screen.getByRole("button", { name: /new workspace/i }));

    expect(useAppStore.getState().workspaceDialog).toEqual({ mode: "create" });
  });

  it("switches panels and marks the active one", async () => {
    const user = setupUser();
    render(<TabBar />);

    expect(screen.getByRole("button", { name: "Workspace" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    await user.click(screen.getByRole("button", { name: "Providers" }));

    expect(useAppStore.getState().activePanel).toBe("providers");
    expect(screen.getByRole("button", { name: "Providers" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("opens the reopen list from the tab bar", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();

    await user.click(openButton());

    expect(switcherMenu()).toBeInTheDocument();
  });
});

describe("TabBar: the reopen list is portal-rendered", () => {
  it("mounts the list in a body-level portal, outside the clipping tab strip", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    const { container } = render(<TabBar />);

    await user.click(openButton());

    // The regression this pins: the list used to be a child of `.tabbar-new`,
    // inside `.tabbar-tabs`, whose `overflow-x: auto` also clips vertically -
    // so the popover rendered outside the strip's box was invisible and
    // unclickable in the running app. jsdom paints nothing and loads no
    // stylesheet, so the clip itself is not observable here; where the popover
    // is *mounted* is, and a body-level portal is what makes the clip
    // irrelevant.
    expect(switcherMenu().parentElement).toBe(document.body);
    expect(container.querySelector(".tabbar-tabs")).not.toContainElement(
      switcherMenu(),
    );
    expect(container.querySelector(".tabbar-new")).not.toContainElement(
      switcherMenu(),
    );
    // A body-level portal has to be positioned in viewport coordinates itself:
    // `fixed`, because `.menu`'s own rule is `position: absolute` anchored to
    // the tab strip's button group, and an inline coordinate pair.
    expect(switcherMenu().style.position).toBe("fixed");
    expect(switcherMenu()).toHaveClass("workspace-switcher");
    expect(switcherMenu().style.left).toMatch(/^\d+px$/);
    expect(switcherMenu().style.top).toMatch(/^\d+px$/);
    expect(switcherMenu().style.visibility).toBe("");
  });

  it("lists every configured workspace, with a truthful open/reopen hint", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());

    expect(screen.getAllByRole("menuitem")).toHaveLength(3);
    expect(within(menuItem(/Alpha/)).getByText("open")).toBeInTheDocument();
    expect(within(menuItem(/Beta/)).getByText("reopen")).toBeInTheDocument();
    expect(within(menuItem(/Gamma/)).getByText("reopen")).toBeInTheDocument();
  });

  it("opens the chosen workspace, starts no session and closes the list", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());
    await user.click(menuItem(/Gamma/));

    const state = useAppStore.getState();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-1", "ws-3"]);
    expect(state.activeTabId).toBe("ws-3");
    // Reopening opens a view; it must not spawn an agent (spec section 14).
    expect(state.sessions["ws-3"]).toBeUndefined();
    expect(invokeMock).not.toHaveBeenCalled();
    // The trigger's own affordance has to follow.
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton()).toHaveAttribute("aria-expanded", "false");
  });

  it("closes again when the trigger is clicked a second time", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());
    expect(openButton()).toHaveAttribute("aria-expanded", "true");

    await user.click(openButton());

    // The trigger counts as "inside" for the click-outside check: without that,
    // the press would close the menu and the click would reopen it, and the
    // button could never dismiss its own list.
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton()).toHaveAttribute("aria-expanded", "false");
  });

  it("closes on Escape", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton()).toHaveAttribute("aria-expanded", "false");
  });

  it("closes on a press outside the list", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());
    await user.click(document.body);

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton()).toHaveAttribute("aria-expanded", "false");
  });

  it("closes when something under it moves, but not when its own list scrolls", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, ["ws-1"]);
    render(<TabBar />);

    await user.click(openButton());

    // A list taller than `max-height` scrolls by design; that is not dismissal.
    fireEvent.scroll(switcherMenu());
    expect(switcherMenu()).toBeInTheDocument();

    // The tab strip scrolling (or the window, or its losing focus) moves the
    // trigger out from under a menu that is placed once, from its rect.
    fireEvent.scroll(document);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton()).toHaveAttribute("aria-expanded", "false");
  });
});

describe("TabBar: the reopen list's placement", () => {
  // jsdom's window is 1024×768 and reports no element sizes, so these tests
  // supply the rects and assert the viewport coordinates the menu is given.
  it("hangs under the trigger when there is room", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, []);
    stubSwitcherRects(
      { left: 100, top: 10, right: 200, bottom: 30 },
      { width: 300, height: 200 },
    );
    render(<TabBar />);

    await user.click(openButton());

    expect(switcherMenu().style.left).toBe("100px");
    expect(switcherMenu().style.top).toBe("34px");
    expect(switcherMenu().style.visibility).toBe("");
  });

  it("right-aligns and flips above the trigger at the bottom-right corner", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, []);
    // A trigger in the corner, where the default placement (below, left-aligned)
    // would put the list off-screen on both axes.
    stubSwitcherRects(
      { left: 900, top: 730, right: 1000, bottom: 750 },
      { width: 300, height: 200 },
    );
    render(<TabBar />);

    await user.click(openButton());

    // 900 + 300 > 1024: right-aligned to the trigger instead (1000 - 300).
    expect(switcherMenu().style.left).toBe("700px");
    // 750 + 4 + 200 > 768, and 730 - 4 - 200 fits: flipped above it.
    expect(switcherMenu().style.top).toBe("526px");
  });

  it("keeps a list larger than the window inside it", async () => {
    const user = setupUser();
    seedWorkspaces(THREE_WORKSPACES, []);
    stubSwitcherRects(
      { left: 900, top: 730, right: 1000, bottom: 750 },
      { width: 1400, height: 900 },
    );
    render(<TabBar />);

    await user.click(openButton());

    // Nothing to flip or align against: the list can only start at the margin,
    // never at a negative coordinate.
    expect(switcherMenu().style.left).toBe("4px");
    expect(switcherMenu().style.top).toBe("4px");
  });
});

describe("WorkspaceSwitcher", () => {
  it("lists every configured workspace, marking the open ones and reopening the closed ones", () => {
    seedWorkspaces(WORKSPACES, ["ws-1"]);
    renderSwitcher();

    const items = screen.getAllByRole("menuitem");
    expect(items).toHaveLength(2);
    expect(within(items[0]).getByText("open")).toBeInTheDocument();
    expect(within(items[1]).getByText("reopen")).toBeInTheDocument();
    expect(items[1]).toHaveAttribute("title", "D:\\Projects\\beta");
  });

  it("reopens a closed workspace in a new tab and closes the list", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, ["ws-1"]);
    const { onClose } = renderSwitcher();

    await user.click(screen.getByRole("menuitem", { name: /Beta/ }));

    const state = useAppStore.getState();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["ws-1", "ws-2"]);
    expect(state.activeTabId).toBe("ws-2");
    // Reopening opens a view; it must not spawn an agent (spec section 14).
    expect(state.sessions["ws-2"]).toBeUndefined();
    expect(invokeMock).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("clicking an already-open workspace activates its tab instead of duplicating it", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, ["ws-1", "ws-2"]);
    useAppStore.getState().setActiveTab("ws-1");

    renderSwitcher();
    await user.click(screen.getByRole("menuitem", { name: /Beta/ }));

    const state = useAppStore.getState();
    expect(state.tabs).toHaveLength(2);
    expect(state.activeTabId).toBe("ws-2");
  });

  it("explains an empty list and a list that is still loading", () => {
    const { unmount } = renderSwitcher();
    expect(screen.getByText("Loading workspaces…")).toBeInTheDocument();
    unmount();

    seedWorkspaces([], []);
    renderSwitcher();
    expect(
      screen.getByText("No workspaces configured yet."),
    ).toBeInTheDocument();
    expect(screen.queryAllByRole("menuitem")).toHaveLength(0);
  });

  it("closes on Escape", async () => {
    const user = setupUser();
    seedWorkspaces(WORKSPACES, []);
    const { onClose } = renderSwitcher();

    await user.keyboard("{Escape}");

    expect(onClose).toHaveBeenCalled();
  });
});
