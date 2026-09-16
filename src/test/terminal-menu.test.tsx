/**
 * The terminal's copy/paste/scroll surface as the user meets it (spec section 11).
 *
 * Two layers are exercised here:
 *
 * - `SessionTerminal` against a fake `Terminal` (`mock-xterm.ts`), which is what
 *   lets a test set a selection, arrange a buffer, fire a keystroke and inspect
 *   what the terminal was asked to do. jsdom has no canvas, so the real xterm
 *   could not answer any of those questions.
 * - `SessionView`'s toolbar Copy button, through the store and the faked Tauri
 *   bridge, to prove the button really reaches the terminal handle.
 *
 * What is *not* testable here, and is not claimed to be: anything a canvas or a
 * layout is needed for - xterm's actual rendering, its own mouse selection, the
 * wheel gesture, the visibility of the styled scrollbar, whether WebView2 grants
 * the clipboard, and the alternate-screen behaviour of a full-screen TUI.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import SessionTerminal from "../components/SessionTerminal";
import SessionView from "../components/SessionView";
import {
  FakeTerminal,
  removeClipboardApi,
  resetXtermMock,
  stubClipboardApi,
  stubExecCommand,
  xtermFitAddonModule,
  xtermModule,
} from "./mock-xterm";
import {
  makeWorkspace,
  resetAppStore,
  seedSession,
  seedWorkspaces,
  setupUser,
  tabFor,
} from "./fixtures";
import { resetTauriMock } from "./mock-tauri";

vi.mock("@xterm/xterm", async () => (await import("./mock-xterm")).xtermModule());
vi.mock("@xterm/addon-fit", async () =>
  (await import("./mock-xterm")).xtermFitAddonModule(),
);
vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);
vi.mock("@tauri-apps/api/event", async () =>
  (await import("./mock-tauri")).tauriEventModule(),
);

// The factories above are referenced through the dynamic import so `vi.mock`'s
// hoisting cannot run them before this module is initialized.
void xtermModule;
void xtermFitAddonModule;

/** The terminal container the user right-clicks. */
function terminalGroup(): HTMLElement {
  return screen.getByRole("group", { name: "Agent session terminal" });
}

/** One menu row, by its stable id (the label carries a shortcut hint too). */
function menuEntry(id: string): HTMLButtonElement {
  const entry = document.querySelector(`[data-menu-entry="${id}"]`);
  if (entry === null) {
    throw new Error(`the context menu has no "${id}" entry`);
  }
  return entry as HTMLButtonElement;
}

/** Right-click the terminal; returns whether the event was prevented. */
function openMenu(x = 40, y = 12): boolean {
  return fireEvent.contextMenu(terminalGroup(), { clientX: x, clientY: y });
}

/** Mount `SessionTerminal` and hand back the fake terminal behind it. */
function renderTerminal(onData?: (data: string) => void): FakeTerminal {
  render(<SessionTerminal onData={onData} />);
  return FakeTerminal.last();
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
  resetXtermMock();
  Reflect.deleteProperty(navigator, "clipboard");
  Reflect.deleteProperty(document, "execCommand");
});

describe("SessionTerminal: the terminal's own options", () => {
  it("keeps a deep scrollback and stays pinned to the bottom while the agent streams", () => {
    renderTerminal();

    const options = FakeTerminal.last().options;
    expect(options.scrollback).toBe(10000);
    expect(options.scrollOnUserInput).toBe(true);
  });

  it("leaves the right button to our menu instead of xterm's select-the-word", () => {
    renderTerminal();

    // xterm's macOS default would replace the selection the menu is about to
    // copy, so the menu is the single owner of the right button.
    expect(FakeTerminal.last().options.rightClickSelectsWord).toBe(false);
  });

  it("themes xterm's own scrollbar, which is what xterm 6 actually paints", () => {
    renderTerminal();

    const theme = FakeTerminal.last().options.theme as Record<string, string>;
    // Same muted palette as `.session-terminal .xterm-viewport::-webkit-scrollbar`
    // in styles.css: --border-strong, --text-faint, --text-dim.
    expect(theme.scrollbarSliderBackground).toBe("#3b4261");
    expect(theme.scrollbarSliderHoverBackground).toBe("#565f89");
    expect(theme.scrollbarSliderActiveBackground).toBe("#7982a9");
  });
});

describe("SessionTerminal: the context menu", () => {
  it("opens at the pointer, with every entry, and never the webview's own menu", () => {
    renderTerminal();

    // `false` is the DOM's "the default action was prevented" - i.e. the
    // platform context menu was suppressed.
    expect(openMenu(40, 12)).toBe(false);

    const menu = screen.getByRole("menu", { name: "Terminal" });
    expect(menu).toHaveStyle({ left: "40px", top: "12px" });
    // Both classes matter: `.menu` supplies the dark styling and `.context-menu`
    // is what positions it at the pointer (see the note in styles.css about the
    // cascade order). Losing either would leave the menu in the wrong place.
    expect(menu).toHaveClass("menu", "context-menu");
    expect(menuEntry("copy")).toHaveTextContent("Copy");
    expect(menuEntry("copy-all")).toHaveTextContent("Copy all");
    expect(menuEntry("paste")).toHaveTextContent("Paste");
    expect(menuEntry("select-all")).toHaveTextContent("Select all");
    expect(menuEntry("scroll-top")).toHaveTextContent("Scroll to top");
    expect(menuEntry("scroll-bottom")).toHaveTextContent("Scroll to bottom");
    expect(menuEntry("clear")).toHaveTextContent("Clear");
  });

  it("greys out Copy when there is no selection, and enables it when there is one", () => {
    const terminal = renderTerminal();

    openMenu();
    expect(menuEntry("copy")).toBeDisabled();
    // Copy all never needs a selection: it copies the buffer.
    expect(menuEntry("copy-all")).toBeEnabled();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Terminal" })).toBeNull();

    terminal.selection = "an agent answer";
    openMenu();

    expect(menuEntry("copy")).toBeEnabled();
  });

  it("copies the selection and closes", async () => {
    const clipboard = stubClipboardApi();
    const terminal = renderTerminal();
    terminal.selection = "the selected lines";

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("copy"));
    });

    expect(clipboard.writeText).toHaveBeenCalledWith("the selected lines");
    expect(screen.queryByRole("menu", { name: "Terminal" })).toBeNull();
  });

  it("copies the whole buffer, scrollback included, with Copy all", async () => {
    const clipboard = stubClipboardApi();
    const terminal = renderTerminal();
    // A trailing blank line and right-hand padding are buffer artefacts, not
    // content the user wants on the clipboard.
    terminal.lines = ["first line", "  indented  ", "", ""];

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("copy-all"));
    });

    expect(clipboard.writeText).toHaveBeenCalledWith("first line\r\n  indented");
  });

  it("says so in the transcript when the clipboard refuses the copy", async () => {
    // Neither clipboard path exists: the copy cannot happen, and a silent
    // no-op is exactly the bug this feature answers.
    removeClipboardApi();
    const terminal = renderTerminal();
    terminal.selection = "unreachable";

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("copy"));
    });

    expect(terminal.writes.join("")).toContain("copy failed");
  });

  it("selects the whole buffer and shows its oldest line", async () => {
    const terminal = renderTerminal();

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("select-all"));
    });

    expect(terminal.selectAllCalls).toBe(1);
    expect(terminal.scrollToTopCalls).toBe(1);
  });

  it("drives the terminal's own scroll API from the scroll entries", async () => {
    const terminal = renderTerminal();

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("scroll-top"));
    });
    expect(terminal.scrollToTopCalls).toBe(1);

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("scroll-bottom"));
    });
    expect(terminal.scrollToBottomCalls).toBe(1);
  });

  it("clears the terminal", async () => {
    const terminal = renderTerminal();

    openMenu();
    await act(async () => {
      fireEvent.click(menuEntry("clear"));
    });

    expect(terminal.clearCalls).toBe(1);
  });

  it("closes on Escape, on an outside press and when the terminal scrolls", () => {
    renderTerminal();

    openMenu();
    expect(screen.queryByRole("menu", { name: "Terminal" })).not.toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Terminal" })).toBeNull();

    openMenu();
    act(() => {
      // A press anywhere else - including a keyboard-driven click on a control.
      window.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    expect(screen.queryByRole("menu", { name: "Terminal" })).toBeNull();

    openMenu();
    act(() => {
      // The terminal's own viewport scrolls without bubbling to the window, so
      // the menu listens in the capture phase; a menu left floating over text
      // that moved would point at the wrong lines.
      fireEvent.scroll(window);
    });
    expect(screen.queryByRole("menu", { name: "Terminal" })).toBeNull();
  });
});

describe("SessionTerminal: the keyboard model reaches the terminal", () => {
  it("consumes Ctrl+C with a selection: the clipboard gets it, the PTY does not", async () => {
    const clipboard = stubClipboardApi();
    const onData = vi.fn();
    const terminal = renderTerminal(onData);
    terminal.selection = "copy me";

    const key = terminal.pressKey({ key: "c", ctrlKey: true });
    await act(async () => {
      await Promise.resolve();
    });

    expect(key.handled).toBe(false);
    expect(key.defaultPrevented).toBe(true);
    expect(clipboard.writeText).toHaveBeenCalledWith("copy me");
    // The whole point: no 0x03 (SIGINT) is sent to the agent.
    expect(onData).not.toHaveBeenCalled();
  });

  it("lets Ctrl+C through - SIGINT - when there is no selection", () => {
    const clipboard = stubClipboardApi();
    const terminal = renderTerminal();

    const key = terminal.pressKey({ key: "c", ctrlKey: true });

    expect(key.handled).toBe(true);
    expect(key.defaultPrevented).toBe(false);
    expect(clipboard.writeText).not.toHaveBeenCalled();
  });

  it("consumes Shift+PageUp and scrolls a page with the terminal's own API", () => {
    const terminal = renderTerminal();

    const key = terminal.pressKey({ key: "PageUp", shiftKey: true });

    expect(key.handled).toBe(false);
    expect(terminal.scrollPagesCalls).toEqual([-1]);
  });

  it("consumes Ctrl+Shift+Home/End and jumps to the oldest line / the bottom", () => {
    const terminal = renderTerminal();

    expect(
      terminal.pressKey({ key: "Home", ctrlKey: true, shiftKey: true }).handled,
    ).toBe(false);
    expect(
      terminal.pressKey({ key: "End", ctrlKey: true, shiftKey: true }).handled,
    ).toBe(false);

    expect(terminal.scrollToTopCalls).toBe(1);
    expect(terminal.scrollToBottomCalls).toBe(1);
  });

  it("leaves a plain keystroke untouched", () => {
    const terminal = renderTerminal();

    const key = terminal.pressKey({ key: "a" });

    expect(key.handled).toBe(true);
    expect(key.defaultPrevented).toBe(false);
    expect(terminal.scrollPagesCalls).toEqual([]);
  });

  it("pastes Ctrl+Shift+V through the normal input path, with PTY line endings", async () => {
    stubClipboardApi(
      async () => undefined,
      async () => "echo one\necho two\r\n",
    );
    const onData = vi.fn();
    const terminal = renderTerminal(onData);

    const key = terminal.pressKey({ key: "v", ctrlKey: true, shiftKey: true });
    await act(async () => {
      await Promise.resolve();
    });

    expect(key.handled).toBe(false);
    expect(onData).toHaveBeenCalledWith("echo one\recho two\r");
  });

  it("brackets a paste when the application asked for bracketed paste", async () => {
    stubClipboardApi(async () => undefined, async () => "line");
    const onData = vi.fn();
    const terminal = renderTerminal(onData);
    terminal.modes.bracketedPasteMode = true;

    terminal.pressKey({ key: "v", ctrlKey: true, shiftKey: true });
    await act(async () => {
      await Promise.resolve();
    });

    expect(onData).toHaveBeenCalledWith("\x1b[200~line\x1b[201~");
  });

  it("does nothing destructive when the clipboard cannot be read", async () => {
    removeClipboardApi();
    const onData = vi.fn();
    const terminal = renderTerminal(onData);

    const key = terminal.pressKey({ key: "v", ctrlKey: true, shiftKey: true });
    await act(async () => {
      await Promise.resolve();
    });

    // Consumed (so the webview does not paste either) but nothing invented:
    // the user still has the untouched native Ctrl+V.
    expect(key.handled).toBe(false);
    expect(onData).not.toHaveBeenCalled();
  });
});

describe("SessionView: the toolbar Copy button", () => {
  /** One open workspace with a session, i.e. a rendered terminal. */
  function openSession(): FakeTerminal {
    seedWorkspaces([makeWorkspace()], ["ws-1"]);
    seedSession("ws-1", { status: "running" });
    render(<SessionView tab={tabFor("ws-1")} />);
    return FakeTerminal.last();
  }

  it("copies the whole terminal when nothing is selected, as its tooltip promises", async () => {
    const user = setupUser();
    const clipboard = stubClipboardApi();
    const terminal = openSession();
    terminal.lines = ["$ npm test", "6 files passed"];

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(clipboard.writeText).toHaveBeenCalledWith("$ npm test\r\n6 files passed");
  });

  it("copies just the selection when there is one", async () => {
    const user = setupUser();
    const clipboard = stubClipboardApi();
    const terminal = openSession();
    terminal.lines = ["$ npm test", "6 files passed"];
    terminal.selection = "6 files passed";

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(clipboard.writeText).toHaveBeenCalledWith("6 files passed");
  });

  it("reports a refused clipboard in the terminal instead of failing silently", async () => {
    const user = setupUser();
    removeClipboardApi();
    const terminal = openSession();

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(terminal.writes.join("")).toContain("copy failed");
  });

  it("still copies through the legacy path when the async API is absent", async () => {
    const user = setupUser();
    removeClipboardApi();
    const execCommand = stubExecCommand();
    const terminal = openSession();
    terminal.lines = ["one"];

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(terminal.writes.join("")).not.toContain("copy failed");
  });
});
