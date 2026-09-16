/**
 * The terminal's keyboard model, as one pure function (spec section 11).
 *
 * xterm calls `attachCustomKeyEventHandler`'s callback for every `keydown`
 * before it does anything with the event; returning `false` means "consumed -
 * do not turn this into agent input". That is the whole reason this module
 * exists: **Ctrl+C must keep sending SIGINT to the PTY**, so copy cannot be
 * implemented by stealing the key outright - it is only copy when there is
 * something to copy.
 *
 * The decision is deliberately separated from the `Terminal` instance: it takes
 * the handful of event fields it reads and an injected action set, so every
 * branch can be tested in jsdom without a canvas, a PTY or a clipboard.
 *
 * | Keystroke                     | Behaviour                                        |
 * | ----------------------------- | ------------------------------------------------ |
 * | `Ctrl+C` with a selection     | copies, consumed (no SIGINT)                     |
 * | `Ctrl+C` with no selection    | untouched - xterm sends `0x03`, the agent's SIGINT |
 * | `Ctrl+Shift+C`                | copies when there is a selection, always consumed |
 * | `Ctrl+Insert`                 | copies when there is a selection, otherwise untouched |
 * | `Ctrl+Shift+V`                | attempts a clipboard read, always consumed        |
 * | `Shift+PageUp` / `PageDown`   | scrolls one page, consumed (never agent input)    |
 * | `Ctrl+Shift+Home` / `End`     | scrolls to the oldest line / back to the bottom   |
 * | anything else                 | untouched                                         |
 *
 * Plain `Ctrl+V`, the mouse wheel and text selection are all left alone: the
 * browser's native paste already reaches the PTY through xterm's own textarea,
 * and xterm's wheel handling is the only thing that knows about the alternate
 * screen buffer (see the caveat in `SessionTerminal.tsx`).
 */

/**
 * The parts of a `KeyboardEvent` this decision reads.
 *
 * A structural subset on purpose: a real `KeyboardEvent` satisfies it (so it
 * can be passed straight through from xterm), and a test can build one with a
 * plain object.
 */
export interface TerminalKeyEvent {
  /** `"keydown"`, `"keyup"`, … - only `keydown` is ours. */
  type: string;
  /** `KeyboardEvent.key`, e.g. `"c"`, `"PageUp"`, `"Insert"`. */
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/**
 * What a consumed keystroke is allowed to do.
 *
 * Injected rather than reached for, which is what keeps the decision testable
 * and the terminal instance out of this module.
 */
export interface TerminalKeyActions {
  /** Whether the terminal currently has a selection to copy. */
  hasSelection(): boolean;
  /** Copy the current selection to the clipboard. */
  copySelection(): void;
  /** Read the clipboard and deliver it as terminal input. */
  pasteFromClipboard(): void;
  /** Scroll one page: `-1` towards the oldest line, `1` towards the bottom. */
  scrollPage(direction: -1 | 1): void;
  /** Scroll to the oldest line in the scrollback. */
  scrollToTop(): void;
  /** Scroll back to the live bottom. */
  scrollToBottom(): void;
}

/**
 * Decide what a keystroke means for the terminal.
 *
 * @returns the value `attachCustomKeyEventHandler` expects: `true` to let xterm
 * process the event (and forward it to the PTY), `false` when it was consumed
 * here and must not become agent input.
 */
export function handleTerminalKey(
  event: TerminalKeyEvent,
  actions: TerminalKeyActions,
): boolean {
  // xterm also calls its custom handler for keyup (and the browser for
  // keypress); consuming those would break key-repeat and IME input.
  if (event.type !== "keydown") {
    return true;
  }
  // Alt/Meta combinations are the user's (or the OS's) shortcuts, and on some
  // layouts `Ctrl+Alt+<letter>` *is* AltGr, i.e. ordinary typing.
  if (event.altKey || event.metaKey) {
    return true;
  }

  // Scrolling. Shift+PageUp/PageDown is what xterm itself would scroll with,
  // but doing it here keeps the amount and the key list in one place, and keeps
  // the keys out of the PTY's reach.
  if (event.shiftKey && !event.ctrlKey) {
    if (event.key === "PageUp") {
      actions.scrollPage(-1);
      return false;
    }
    if (event.key === "PageDown") {
      actions.scrollPage(1);
      return false;
    }
  }

  if (event.ctrlKey && event.shiftKey) {
    if (event.key === "Home") {
      actions.scrollToTop();
      return false;
    }
    if (event.key === "End") {
      actions.scrollToBottom();
      return false;
    }
    // Ctrl+Shift+C is an explicit copy gesture. It is consumed even with no
    // selection: as a keystroke it is not "interrupt the agent", and letting it
    // through would send a `^C` the user did not ask for.
    if (event.key === "c" || event.key === "C") {
      if (actions.hasSelection()) {
        actions.copySelection();
      }
      return false;
    }
    // Same gesture, older binding (also handled for its shifted form below).
    if (event.key === "Insert" && actions.hasSelection()) {
      actions.copySelection();
      return false;
    }
    // Ctrl+Shift+V is consumed so the webview's own "paste as plain text" does
    // not also fire; the paste itself is attempted by the action.
    if (event.key === "v" || event.key === "V") {
      actions.pasteFromClipboard();
      return false;
    }
    return true;
  }

  // Ctrl+C: copy only when there is something to copy. With no selection this
  // falls through to xterm, which sends `0x03` - the agent's SIGINT, and a
  // behaviour this terminal must never change.
  if (event.ctrlKey && (event.key === "c" || event.key === "C")) {
    if (actions.hasSelection()) {
      actions.copySelection();
      return false;
    }
    return true;
  }

  // Ctrl+Insert copies when there is a selection; with none it is left to xterm.
  if (event.ctrlKey && event.key === "Insert" && actions.hasSelection()) {
    actions.copySelection();
    return false;
  }

  return true;
}
