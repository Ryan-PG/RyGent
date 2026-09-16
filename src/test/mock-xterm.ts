/**
 * Fake `@xterm/xterm` for the terminal tests.
 *
 * jsdom has no canvas, so the real `Terminal` paints nothing and - more to the
 * point - a test cannot reach into it to set a selection, arrange a buffer or
 * fire a keystroke. The terminal's own copy/paste/scroll wiring needs exactly
 * those hooks, so the *module* is replaced here (the historical `mock-tauri.ts`
 * convention: fake the boundary, keep everything else real) and the component,
 * its key decisions and `services/clipboard.ts` all run their real code.
 *
 * Deliberately dumb: it records what it was asked to do and answers what a test
 * told it to answer. Two consequences worth knowing:
 *
 * - `fit()` is never reached in a test, because `SessionTerminal.safeFit()`
 *   refuses to fit a zero-sized element and jsdom lays nothing out. That is a
 *   jsdom limitation, not a gap in the fake.
 * - The alternate screen buffer is only a flag here (`bufferType`); no test
 *   claims anything about how a full-screen TUI behaves (see the caveat in
 *   `SessionTerminal.tsx` and in the README).
 *
 * Usage in a test file (the factories are read through a dynamic `import` so
 * `vi.mock`'s hoisting cannot see them before this module is initialized):
 *
 *   vi.mock("@xterm/xterm", async () => (await import("./mock-xterm")).xtermModule());
 *   vi.mock("@xterm/addon-fit", async () => (await import("./mock-xterm")).xtermFitAddonModule());
 */

import { vi } from "vitest";

/** One line of the fake buffer, shaped like `IBufferLine`. */
interface FakeBufferLine {
  translateToString(trimRight?: boolean): string;
}

/** The fields of the fake buffer this suite actually reads. */
export interface FakeBuffer {
  active: {
    type: "normal" | "alternate";
    length: number;
    getLine(index: number): FakeBufferLine | undefined;
  };
}

/** A keystroke, as `handleTerminalKey` reads it. */
export interface FakeKeyEvent {
  type?: string;
  key?: string;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
}

/** Standing in for the real `Terminal`; see this module's header. */
export class FakeTerminal {
  /** Every instance built since the last `resetXtermMock()`. */
  static instances: FakeTerminal[] = [];

  /** The most recent instance - i.e. the one the component under test drives. */
  static last(): FakeTerminal {
    const terminal = FakeTerminal.instances[FakeTerminal.instances.length - 1];
    if (terminal === undefined) {
      throw new Error("no Terminal has been constructed");
    }
    return terminal;
  }

  /** The options the component constructed this terminal with. */
  readonly options: Record<string, unknown>;

  cols = 80;
  rows = 24;

  /** What `hasSelection()`/`getSelection()` answer. */
  selection = "";

  /** The active buffer's lines, oldest first. */
  lines: string[] = [];

  /** Which buffer is active; `alternate` is a full-screen TUI. */
  bufferType: "normal" | "alternate" = "normal";

  modes = { bracketedPasteMode: false };

  /** The element `open()` was called with (the mount node). */
  openedIn: HTMLElement | null = null;

  /** Registered callbacks, so a test can deliver data/resize/keys itself. */
  dataHandler: ((data: string) => void) | null = null;
  resizeHandler: ((size: { cols: number; rows: number }) => void) | null = null;
  keyHandler: ((event: KeyboardEvent) => boolean) | null = null;

  /** Recorded effects. */
  readonly writes: string[] = [];
  readonly scrollPagesCalls: number[] = [];
  readonly addons: unknown[] = [];
  scrollToTopCalls = 0;
  scrollToBottomCalls = 0;
  selectAllCalls = 0;
  clearCalls = 0;
  focusCalls = 0;
  disposed = false;

  constructor(options: Record<string, unknown> = {}) {
    this.options = options;
    FakeTerminal.instances.push(this);
  }

  get buffer(): FakeBuffer {
    const terminal = this;
    return {
      active: {
        type: terminal.bufferType,
        length: terminal.lines.length,
        getLine: (index) => {
          const text = terminal.lines[index];
          if (text === undefined) {
            return undefined;
          }
          return {
            translateToString: (trimRight = false) =>
              trimRight ? text.replace(/\s+$/, "") : text,
          };
        },
      },
    };
  }

  loadAddon(addon: unknown): void {
    this.addons.push(addon);
  }

  open(element: HTMLElement): void {
    this.openedIn = element;
  }

  onData(handler: (data: string) => void): { dispose(): void } {
    this.dataHandler = handler;
    return {
      dispose: () => {
        this.dataHandler = null;
      },
    };
  }

  onResize(handler: (size: { cols: number; rows: number }) => void): {
    dispose(): void;
  } {
    this.resizeHandler = handler;
    return {
      dispose: () => {
        this.resizeHandler = null;
      },
    };
  }

  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void {
    this.keyHandler = handler;
  }

  write(data: string): void {
    this.writes.push(data);
  }

  writeln(data: string): void {
    this.writes.push(`${data}\n`);
  }

  clear(): void {
    this.clearCalls += 1;
  }

  reset(): void {
    this.clearCalls += 1;
  }

  focus(): void {
    this.focusCalls += 1;
  }

  hasSelection(): boolean {
    return this.selection.length > 0;
  }

  getSelection(): string {
    return this.selection;
  }

  selectAll(): void {
    this.selectAllCalls += 1;
  }

  scrollPages(count: number): void {
    this.scrollPagesCalls.push(count);
  }

  scrollToTop(): void {
    this.scrollToTopCalls += 1;
  }

  scrollToBottom(): void {
    this.scrollToBottomCalls += 1;
  }

  dispose(): void {
    this.disposed = true;
  }

  /**
   * Deliver a keystroke the way xterm does.
   *
   * @returns what the terminal's custom handler answered (`false` = consumed)
   * and whether it also prevented the browser's default action.
   */
  pressKey(partial: FakeKeyEvent): { handled: boolean; defaultPrevented: boolean } {
    if (this.keyHandler === null) {
      throw new Error("the component attached no custom key handler");
    }
    let defaultPrevented = false;
    const event = {
      type: "keydown",
      key: "",
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      metaKey: false,
      preventDefault: () => {
        defaultPrevented = true;
      },
      ...partial,
    };
    const handled = this.keyHandler(event as unknown as KeyboardEvent);
    return { handled, defaultPrevented };
  }
}

/** Standing in for the real `FitAddon`. */
export class FakeFitAddon {
  static instances: FakeFitAddon[] = [];

  fitCalls = 0;

  fit(): void {
    this.fitCalls += 1;
  }
}

/** Fake `@xterm/xterm`. */
export function xtermModule(): Record<string, unknown> {
  return { Terminal: FakeTerminal };
}

/** Fake `@xterm/addon-fit`. */
export function xtermFitAddonModule(): Record<string, unknown> {
  return { FitAddon: FakeFitAddon };
}

/** Forget every terminal/addon built so far. */
export function resetXtermMock(): void {
  FakeTerminal.instances.length = 0;
  FakeFitAddon.instances.length = 0;
}

/**
 * Install an async Clipboard API on `navigator`, or remove it.
 *
 * jsdom implements neither `navigator.clipboard` nor
 * `document.execCommand("copy")`, so which path a test exercises has to be
 * stated explicitly rather than inherited from the environment.
 */
export function stubClipboardApi(
  writeText: (text: string) => Promise<void> = async () => undefined,
  readText: () => Promise<string> = async () => "",
): { writeText: ReturnType<typeof vi.fn>; readText: ReturnType<typeof vi.fn> } {
  const clipboard = {
    writeText: vi.fn(writeText),
    readText: vi.fn(readText),
  };
  Object.defineProperty(navigator, "clipboard", {
    value: clipboard,
    configurable: true,
  });
  return clipboard;
}

/** Make `navigator.clipboard` look like a webview that does not expose it. */
export function removeClipboardApi(): void {
  Object.defineProperty(navigator, "clipboard", {
    value: undefined,
    configurable: true,
  });
}

/**
 * Install the legacy copy path, which jsdom does not implement.
 *
 * @param result what `document.execCommand("copy")` answers (default: it works).
 */
export function stubExecCommand(result = true): ReturnType<typeof vi.fn> {
  const execCommand = vi.fn(() => result);
  Object.defineProperty(document, "execCommand", {
    value: execCommand,
    configurable: true,
    writable: true,
  });
  return execCommand;
}
