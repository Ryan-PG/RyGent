/**
 * The terminal's keyboard model (spec section 11).
 *
 * `handleTerminalKey` is the whole decision, so it is tested as a pure function:
 * the event is a plain object and the actions are spies. That is what lets these
 * tests state the contract that matters most - **Ctrl+C without a selection is
 * still SIGINT** - without a PTY, a canvas or a browser.
 *
 * The component wiring (that xterm's custom handler is installed and calls this)
 * is covered in `terminal-menu.test.tsx`; the clipboard itself in
 * `clipboard.test.ts`.
 */

import { describe, expect, it, vi } from "vitest";
import {
  handleTerminalKey,
  type TerminalKeyActions,
  type TerminalKeyEvent,
} from "../components/terminalKeys";

/** Spies for everything a consumed keystroke may do. */
function makeActions(hasSelection = false): TerminalKeyActions {
  return {
    hasSelection: vi.fn(() => hasSelection),
    copySelection: vi.fn(),
    pasteFromClipboard: vi.fn(),
    scrollPage: vi.fn(),
    scrollToTop: vi.fn(),
    scrollToBottom: vi.fn(),
  };
}

/** A keydown, with every modifier explicitly off unless a test asks for it. */
function makeEvent(overrides: Partial<TerminalKeyEvent>): TerminalKeyEvent {
  return {
    type: "keydown",
    key: "",
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    ...overrides,
  };
}

describe("handleTerminalKey: Ctrl+C", () => {
  it("copies and is consumed when there is a selection", () => {
    const actions = makeActions(true);

    const processed = handleTerminalKey(
      makeEvent({ key: "c", ctrlKey: true }),
      actions,
    );

    // `false` is "consumed": xterm must not turn this into a 0x03 byte.
    expect(processed).toBe(false);
    expect(actions.copySelection).toHaveBeenCalledTimes(1);
    expect(actions.pasteFromClipboard).not.toHaveBeenCalled();
    expect(actions.scrollPage).not.toHaveBeenCalled();
  });

  it("passes through untouched - SIGINT - when there is no selection", () => {
    const actions = makeActions(false);

    const processed = handleTerminalKey(
      makeEvent({ key: "c", ctrlKey: true }),
      actions,
    );

    expect(processed).toBe(true);
    expect(actions.copySelection).not.toHaveBeenCalled();
  });

  it("treats an uppercase C the same way (Shift is ignored for Ctrl+letters)", () => {
    const actions = makeActions(true);

    expect(
      handleTerminalKey(makeEvent({ key: "C", ctrlKey: true }), actions),
    ).toBe(false);
    expect(actions.copySelection).toHaveBeenCalledTimes(1);
  });

  it("is not ours on keyup, so key-repeat and IME input stay intact", () => {
    const actions = makeActions(true);

    const processed = handleTerminalKey(
      makeEvent({ type: "keyup", key: "c", ctrlKey: true }),
      actions,
    );

    expect(processed).toBe(true);
    expect(actions.copySelection).not.toHaveBeenCalled();
  });
});

describe("handleTerminalKey: the explicit copy gestures", () => {
  it("copies on Ctrl+Shift+C and consumes the keystroke", () => {
    const actions = makeActions(true);

    const processed = handleTerminalKey(
      makeEvent({ key: "c", ctrlKey: true, shiftKey: true }),
      actions,
    );

    expect(processed).toBe(false);
    expect(actions.copySelection).toHaveBeenCalledTimes(1);
  });

  it("still consumes Ctrl+Shift+C with no selection, and copies nothing", () => {
    const actions = makeActions(false);

    const processed = handleTerminalKey(
      makeEvent({ key: "c", ctrlKey: true, shiftKey: true }),
      actions,
    );

    // Consumed because it is a copy gesture, not an interrupt: passing it on
    // would send the agent a ^C the user never asked for.
    expect(processed).toBe(false);
    expect(actions.copySelection).not.toHaveBeenCalled();
  });

  it("copies on Ctrl+Insert when there is a selection", () => {
    const actions = makeActions(true);

    expect(
      handleTerminalKey(makeEvent({ key: "Insert", ctrlKey: true }), actions),
    ).toBe(false);
    expect(actions.copySelection).toHaveBeenCalledTimes(1);
  });

  it("leaves Ctrl+Insert alone when there is nothing selected", () => {
    const actions = makeActions(false);

    expect(
      handleTerminalKey(makeEvent({ key: "Insert", ctrlKey: true }), actions),
    ).toBe(true);
    expect(actions.copySelection).not.toHaveBeenCalled();
  });
});

describe("handleTerminalKey: pasting", () => {
  it("reads the clipboard on Ctrl+Shift+V and consumes the keystroke", () => {
    const actions = makeActions();

    const processed = handleTerminalKey(
      makeEvent({ key: "v", ctrlKey: true, shiftKey: true }),
      actions,
    );

    // Consumed so the webview's own "paste as plain text" cannot fire as well.
    expect(processed).toBe(false);
    expect(actions.pasteFromClipboard).toHaveBeenCalledTimes(1);
  });

  it("leaves the native Ctrl+V alone", () => {
    const actions = makeActions();

    expect(
      handleTerminalKey(makeEvent({ key: "v", ctrlKey: true }), actions),
    ).toBe(true);
    expect(actions.pasteFromClipboard).not.toHaveBeenCalled();
  });
});

describe("handleTerminalKey: scrolling", () => {
  it("scrolls a page up on Shift+PageUp, without reaching the PTY", () => {
    const actions = makeActions();

    const processed = handleTerminalKey(
      makeEvent({ key: "PageUp", shiftKey: true }),
      actions,
    );

    expect(processed).toBe(false);
    expect(actions.scrollPage).toHaveBeenCalledWith(-1);
  });

  it("scrolls a page down on Shift+PageDown", () => {
    const actions = makeActions();

    const processed = handleTerminalKey(
      makeEvent({ key: "PageDown", shiftKey: true }),
      actions,
    );

    expect(processed).toBe(false);
    expect(actions.scrollPage).toHaveBeenCalledWith(1);
  });

  it("jumps to the oldest line on Ctrl+Shift+Home and back on Ctrl+Shift+End", () => {
    const actions = makeActions();

    expect(
      handleTerminalKey(
        makeEvent({ key: "Home", ctrlKey: true, shiftKey: true }),
        actions,
      ),
    ).toBe(false);
    expect(
      handleTerminalKey(
        makeEvent({ key: "End", ctrlKey: true, shiftKey: true }),
        actions,
      ),
    ).toBe(false);

    expect(actions.scrollToTop).toHaveBeenCalledTimes(1);
    expect(actions.scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it("leaves plain PageUp/PageDown to the application", () => {
    const actions = makeActions();

    expect(handleTerminalKey(makeEvent({ key: "PageUp" }), actions)).toBe(true);
    expect(handleTerminalKey(makeEvent({ key: "PageDown" }), actions)).toBe(true);
    expect(actions.scrollPage).not.toHaveBeenCalled();
  });

  it("leaves Shift+Home alone (that is selection, not scrolling)", () => {
    const actions = makeActions();

    expect(handleTerminalKey(makeEvent({ key: "Home", shiftKey: true }), actions)).toBe(
      true,
    );
    expect(actions.scrollToTop).not.toHaveBeenCalled();
  });
});

describe("handleTerminalKey: everything else", () => {
  it.each([
    ["a plain letter", { key: "a" }],
    ["a bare c", { key: "c" }],
    ["Ctrl+A", { key: "a", ctrlKey: true }],
    ["Enter", { key: "Enter" }],
    ["an arrow key", { key: "ArrowUp" }],
    ["Ctrl+Shift+A (not ours)", { key: "a", ctrlKey: true, shiftKey: true }],
    ["Ctrl+Alt+C (AltGr on many layouts)", { key: "c", ctrlKey: true, altKey: true }],
    ["Ctrl+Escape", { key: "Escape", ctrlKey: true }],
    ["Meta+C (the OS's shortcut)", { key: "c", metaKey: true }],
  ] as [string, Partial<TerminalKeyEvent>][])(
    "leaves %s untouched",
    (_label, overrides) => {
      const actions = makeActions(true);

      expect(handleTerminalKey(makeEvent(overrides), actions)).toBe(true);
      expect(actions.copySelection).not.toHaveBeenCalled();
      expect(actions.pasteFromClipboard).not.toHaveBeenCalled();
      expect(actions.scrollPage).not.toHaveBeenCalled();
      expect(actions.scrollToTop).not.toHaveBeenCalled();
      expect(actions.scrollToBottom).not.toHaveBeenCalled();
    },
  );
});
