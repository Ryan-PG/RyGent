/**
 * The embedded terminal (spec section 11).
 *
 * Wraps `@xterm/xterm` in the minimum React glue: the `Terminal` instance is
 * created once per mount and never re-created, because re-creating it would
 * discard the scrollback and detach the running PTY. Callbacks therefore live
 * in a ref (`callbacksRef`) instead of the mount effect's dependency list.
 *
 * The component is deliberately dumb about sessions: it renders bytes and
 * forwards keystrokes/resizes. Session lifecycle, commands and status live in
 * `SessionView` + the store.
 *
 * What this component does own is the terminal's *keyboard, mouse and clipboard*
 * model (the keys are decided in `terminalKeys.ts`, the clipboard in
 * `services/clipboard.ts`):
 *
 * - **Copy** - `Ctrl+C` copies when there is a selection and is deliberately
 *   still `SIGINT` when there is not; `Ctrl+Shift+C` and `Ctrl+Insert` are the
 *   unconditional gestures; right-click opens a menu with copy/paste/scroll.
 * - **Paste** - the browser's native `Ctrl+V` is untouched (xterm's textarea
 *   delivers it to the PTY); `Ctrl+Shift+V` reads the clipboard ourselves.
 * - **Scroll** - `scrollback` plus `Shift+PageUp/PageDown` and
 *   `Ctrl+Shift+Home/End`; the mouse wheel is left entirely to xterm.
 *
 * ## The alternate-screen caveat (not a bug, and not worked around)
 *
 * When the agent takes over the alternate screen buffer - a full-screen TUI -
 * the terminal has **no local scrollback by design**: the application owns the
 * screen, and xterm reports wheel events to it as arrow keys by default. So in
 * that mode there is nothing of ours to scroll, and this component does not
 * fake it: `scrollToTop`/`scrollToBottom`/`Shift+PageUp` do nothing useful
 * there because the terminal's buffer holds exactly one screen. Scrolling the
 * *application's* content is the application's own business.
 */

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type Ref,
} from "react";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  handleTerminalKey,
  type TerminalKeyActions,
} from "./terminalKeys";
import TerminalContextMenu, {
  type ContextMenuEntry,
} from "./TerminalContextMenu";
import { copyText, readClipboardText } from "../services/clipboard";
import "@xterm/xterm/css/xterm.css";

/**
 * Tokyo-night-ish palette copied from the `:root` custom properties in
 * `styles.css`. Duplicated as literals on purpose: xterm paints to a canvas and
 * cannot resolve CSS variables.
 *
 * The three `scrollbarSlider*` entries are the same duplication for xterm 6's
 * own overlay scrollbar, which is themed from here (it injects a `<style>` rule
 * for `.xterm-scrollable-element > .scrollbar > .slider`), not from our
 * stylesheet. `scrollbarSliderBackground` is `--border-strong`, hover is
 * `--gray`/`--text-faint` and the drag state is `--text-dim`.
 */
const TERMINAL_THEME: ITheme = {
  background: "#1a1b26",
  foreground: "#c0caf5",
  cursor: "#7aa2f7",
  cursorAccent: "#1a1b26",
  selectionBackground: "#3b4261",
  scrollbarSliderBackground: "#3b4261",
  scrollbarSliderHoverBackground: "#565f89",
  scrollbarSliderActiveBackground: "#7982a9",
  black: "#16161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#c0caf5",
  brightBlack: "#565f89",
  brightRed: "#f7768e",
  brightGreen: "#9ece6a",
  brightYellow: "#e0af68",
  brightBlue: "#7aa2f7",
  brightMagenta: "#bb9af7",
  brightCyan: "#7dcfff",
  brightWhite: "#c0caf5",
};

/** Matches `--font-mono` so the terminal does not look bolted on. */
const TERMINAL_FONT_FAMILY =
  '"Cascadia Code", "Cascadia Mono", Consolas, "SF Mono", "DejaVu Sans Mono", monospace';

/**
 * Lines of scrollback kept per terminal.
 *
 * Generous on purpose: a coding agent's transcript is long, and the wheel plus
 * `Shift+PageUp`/`Ctrl+Shift+Home` are the only way back through it.
 */
const TERMINAL_SCROLLBACK = 10000;

/** Imperative surface a parent drives the terminal with. */
export interface SessionTerminalHandle {
  /** Write raw PTY output (may contain ANSI sequences). */
  write(data: string): void;
  /** Write a line, appending a newline. */
  writeln(data: string): void;
  /** Clear the scrollback and viewport. */
  clear(): void;
  /** Clear and reset every terminal mode/colour (spec: hard reset). */
  reset(): void;
  /** Give the terminal keyboard focus. */
  focus(): void;
  /** Re-measure the container; safe to call at any time. */
  fit(): void;
  /**
   * Write a dim, bracketed annotation line.
   *
   * Used for local UI messages (command failures, the mock-mode banner) that
   * are not part of the agent's own output stream, so they are visually
   * distinguishable from it.
   */
  writeStatus(message: string): void;
  /** Current terminal dimensions, or `null` if the terminal is not mounted. */
  size(): { cols: number; rows: number } | null;
  /**
   * Copy the current selection to the clipboard.
   *
   * @returns `false` when there was no selection, or when every clipboard
   * strategy failed - the caller is expected to say so rather than assume.
   */
  copySelection(): Promise<boolean>;
  /** Copy the whole buffer (scrollback included), whatever is selected. */
  copyAll(): Promise<boolean>;
  /** Select the whole buffer - what `copyAll` copies. */
  selectAll(): void;
  /** Scroll to the oldest line still in the scrollback. */
  scrollToTop(): void;
  /** Scroll back to the live bottom (this is where new output is). */
  scrollToBottom(): void;
  /** Whether there is currently a selection to copy. */
  hasSelection(): boolean;
}

interface Props {
  /** Forwarded keystrokes (raw, as xterm reports them). */
  onData?: (data: string) => void;
  /** Fired whenever the fitted size changes, so the PTY can be resized. */
  onResize?: (cols: number, rows: number) => void;
  /** Called once, after the terminal exists - an alternative to the ref. */
  onReady?: (handle: SessionTerminalHandle) => void;
  /** Extra class for layout (the viewport sizing lives in `styles.css`). */
  className?: string;
}

/** Where the pointer opened the context menu, in viewport coordinates. */
interface MenuPosition {
  x: number;
  y: number;
}

function SessionTerminal(
  { onData, onResize, onReady, className }: Props,
  ref: Ref<SessionTerminalHandle>,
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);

  // Latest callbacks, so the mount effect can stay dependency-free.
  const callbacksRef = useRef({ onData, onResize, onReady });
  useEffect(() => {
    callbacksRef.current = { onData, onResize, onReady };
  });

  /** Open context menu; `null` while closed. State, so it survives no re-render rules. */
  const [menu, setMenu] = useState<MenuPosition | null>(null);

  /**
   * Fit only when the container actually has a size.
   *
   * `FitAddon.fit()` divides by the measured cell size and `parseInt`s computed
   * style, which yields `NaN` for a hidden or not-yet-laid-out container (the
   * very first render, or a tab that is mounted while another panel is shown).
   * FitAddon guards the `NaN` case internally by returning early, but a stale
   * measurement could still be resized into, so the check happens here too and
   * the ResizeObserver retries as soon as a real size exists.
   *
   * @returns whether a fit was attempted.
   */
  function safeFit(): boolean {
    const element = containerRef.current;
    const terminal = termRef.current;
    const fitAddon = fitAddonRef.current;
    if (!element || !terminal || !fitAddon) {
      return false;
    }
    if (element.clientWidth === 0 || element.clientHeight === 0) {
      return false;
    }
    try {
      fitAddon.fit();
      return true;
    } catch {
      // Layout was not measurable after all; the next resize retries.
      return false;
    }
  }

  /**
   * Copy `text`, narrating a failure on the terminal itself.
   *
   * A copy that silently does nothing is exactly the complaint this feature
   * answers, so a refused clipboard (unfocused webview, no permission) says so
   * in the transcript instead of looking like a lost selection.
   */
  const copyAndReport = useCallback(
    async (text: string): Promise<boolean> => {
      const copied = await copyText(text);
      if (!copied) {
        termRef.current?.write(
          "\r\n\x1b[2m[copy failed: the clipboard is not available]\x1b[0m\r\n",
        );
      }
      return copied;
    },
    [],
  );

  /**
   * Every line of the buffer, scrollback included.
   *
   * `translateToString(true)` trims each line's right-hand padding, which is
   * what makes a copied block paste cleanly instead of carrying a rectangle of
   * spaces. In the alternate screen buffer this is one screen and no
   * scrollback - see the caveat in this file's header.
   */
  const readWholeBuffer = useCallback((): string => {
    const terminal = termRef.current;
    if (!terminal) {
      return "";
    }
    const buffer = terminal.buffer.active;
    const lines: string[] = [];
    for (let index = 0; index < buffer.length; index += 1) {
      lines.push(buffer.getLine(index)?.translateToString(true) ?? "");
    }
    // Trailing blank lines are an artefact of the buffer, not user content.
    while (lines.length > 0 && lines[lines.length - 1] === "") {
      lines.pop();
    }
    return lines.join("\r\n");
  }, []);

  /**
   * The imperative handle is built once and reads everything through refs, so
   * its identity never changes and `useImperativeHandle` never re-publishes it.
   */
  const handleRef = useRef<SessionTerminalHandle | null>(null);
  if (handleRef.current === null) {
    handleRef.current = {
      write: (data) => termRef.current?.write(data),
      writeln: (data) => termRef.current?.writeln(data),
      clear: () => termRef.current?.clear(),
      reset: () => termRef.current?.reset(),
      focus: () => termRef.current?.focus(),
      fit: () => safeFit(),
      writeStatus: (message) =>
        termRef.current?.write(`\r\n\x1b[2m[${message}]\x1b[0m\r\n`),
      size: () => {
        const terminal = termRef.current;
        return terminal ? { cols: terminal.cols, rows: terminal.rows } : null;
      },
      copySelection: () => {
        const terminal = termRef.current;
        if (!terminal || !terminal.hasSelection()) {
          // Not an error: there is simply nothing selected yet.
          return Promise.resolve(false);
        }
        return copyAndReport(terminal.getSelection());
      },
      copyAll: () => copyAndReport(readWholeBuffer()),
      selectAll: () => {
        termRef.current?.selectAll();
        // Put the scrollback's oldest line on screen, so "Select all" visibly
        // does something even when the selection starts off-screen.
        termRef.current?.scrollToTop();
      },
      scrollToTop: () => termRef.current?.scrollToTop(),
      scrollToBottom: () => termRef.current?.scrollToBottom(),
      hasSelection: () => termRef.current?.hasSelection() ?? false,
    };
  }
  useImperativeHandle(ref, () => handleRef.current as SessionTerminalHandle, []);

  /**
   * The keyboard actions handed to `terminalKeys.ts`, built once for the same
   * reason the handle is: xterm's custom handler is installed on the instance
   * and must outlive every render.
   */
  const keyActionsRef = useRef<TerminalKeyActions | null>(null);
  if (keyActionsRef.current === null) {
    keyActionsRef.current = {
      hasSelection: () => termRef.current?.hasSelection() ?? false,
      copySelection: () => {
        void handleRef.current?.copySelection();
      },
      pasteFromClipboard: () => {
        void pasteFromClipboard();
      },
      scrollPage: (direction) => termRef.current?.scrollPages(direction),
      scrollToTop: () => termRef.current?.scrollToTop(),
      scrollToBottom: () => termRef.current?.scrollToBottom(),
    };
  }

  /**
   * `Ctrl+Shift+V`: read the clipboard ourselves and hand it to the session's
   * input path.
   *
   * Line endings are normalised the way xterm's own paste does (`\n` -> `\r`),
   * because the PTY expects carriage returns. When the read is unavailable this
   * does nothing at all - deliberately not "paste nothing" as a destructive
   * empty write - and the user still has the untouched native `Ctrl+V`.
   */
  async function pasteFromClipboard(): Promise<void> {
    const text = await readClipboardText();
    if (text === null || text.length === 0) {
      return;
    }
    const terminal = termRef.current;
    const normalized = text.replace(/\r?\n/g, "\r");
    // Bracketed paste, when the application asked for it, so a multi-line paste
    // is not executed line by line by an editor or a TUI.
    const payload = terminal?.modes.bracketedPasteMode
      ? `\x1b[200~${normalized}\x1b[201~`
      : normalized;
    callbacksRef.current.onData?.(payload);
  }

  useEffect(() => {
    const element = containerRef.current;
    if (!element) {
      return;
    }

    const terminal = new Terminal({
      theme: TERMINAL_THEME,
      fontFamily: TERMINAL_FONT_FAMILY,
      fontSize: 12,
      lineHeight: 1.2,
      // The PTY sends \r\n itself; translating them again would double-space
      // every line of a full-screen TUI.
      convertEol: false,
      scrollback: TERMINAL_SCROLLBACK,
      cursorBlink: true,
      // Keeps the view pinned to the bottom while the agent streams, which is
      // what an interactive CLI session expects.
      scrollOnUserInput: true,
      // Our own right-click menu owns the right button. xterm's macOS default
      // (select the word under the cursor) would replace the very selection the
      // user is about to copy from that menu.
      rightClickSelectsWord: false,
    });
    const fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    termRef.current = terminal;
    fitAddonRef.current = fitAddon;

    // Copy/scroll decisions, and the only place a keystroke is stopped from
    // reaching the PTY. `preventDefault` stops the webview's own default action
    // for a key we consumed (e.g. "paste as plain text" for Ctrl+Shift+V).
    terminal.attachCustomKeyEventHandler((event) => {
      const actions = keyActionsRef.current;
      if (actions === null) {
        return true;
      }
      const keep = handleTerminalKey(event, actions);
      if (!keep) {
        event.preventDefault();
      }
      return keep;
    });

    // `open` must happen for a Terminal to have an `element`; it is tolerant of
    // a zero-sized parent, so a tab opened while hidden still mounts.
    terminal.open(element);
    const fitted = safeFit();

    const dataSub = terminal.onData((data) => {
      callbacksRef.current.onData?.(data);
    });
    const resizeSub = terminal.onResize(({ cols, rows }) => {
      callbacksRef.current.onResize?.(cols, rows);
    });

    const observer =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => safeFit())
        : null;
    observer?.observe(element);

    // Report the fitted size once, so a parent that missed the first
    // `onResize` (it subscribes after this effect) still learns the geometry.
    // Only when the fit actually ran - otherwise the values are still xterm's
    // 80x24 defaults and the observer reports the real size a moment later.
    const handle = handleRef.current;
    if (handle) {
      callbacksRef.current.onReady?.(handle);
      const size = fitted ? handle.size() : null;
      if (size) {
        callbacksRef.current.onResize?.(size.cols, size.rows);
      }
    }

    return () => {
      observer?.disconnect();
      dataSub.dispose();
      resizeSub.dispose();
      terminal.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
    // Mount-only: the terminal instance must outlive every re-render, which is
    // also why `safeFit` and the callbacks are read through refs.
  }, []);

  /**
   * Right-click: our menu, never the webview's.
   *
   * `preventDefault` is what suppresses the platform menu (WebView2's
   * "Reload / Inspect" surface, which is useless against a canvas); xterm has
   * already focused its textarea by this point, so the terminal keeps the
   * keyboard.
   */
  const openMenu = useCallback((event: ReactMouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    setMenu({ x: event.clientX, y: event.clientY });
  }, []);

  const closeMenu = useCallback(() => setMenu(null), []);

  // Rebuilt on each render so the enabled state matches the selection at the
  // moment the menu is opened - the menu is closed before any of these runs, so
  // a stale closure cannot be clicked.
  const menuEntries: ContextMenuEntry[] = menu
    ? [
        {
          id: "copy",
          label: "Copy",
          hint: "Ctrl+C",
          group: 0,
          disabled: !(handleRef.current?.hasSelection() ?? false),
          run: () => void handleRef.current?.copySelection(),
        },
        {
          id: "copy-all",
          label: "Copy all",
          group: 0,
          run: () => void handleRef.current?.copyAll(),
        },
        {
          id: "paste",
          label: "Paste",
          hint: "Ctrl+Shift+V",
          group: 1,
          run: () => void pasteFromClipboard(),
        },
        {
          id: "select-all",
          label: "Select all",
          group: 1,
          run: () => handleRef.current?.selectAll(),
        },
        {
          id: "scroll-top",
          label: "Scroll to top",
          hint: "Ctrl+Shift+Home",
          group: 2,
          run: () => handleRef.current?.scrollToTop(),
        },
        {
          id: "scroll-bottom",
          label: "Scroll to bottom",
          hint: "Ctrl+Shift+End",
          group: 2,
          run: () => handleRef.current?.scrollToBottom(),
        },
        {
          id: "clear",
          label: "Clear",
          group: 3,
          run: () => handleRef.current?.clear(),
        },
      ]
    : [];

  return (
    <div
      className={
        className ? `session-terminal ${className}` : "session-terminal"
      }
      role="group"
      aria-label="Agent session terminal"
      onContextMenu={openMenu}
    >
      {/* The xterm mount node: it must stay a plain, empty element, because
          `open()` measures its padding and the whole terminal lives inside it.
          `useImperativeHandle`/refs keep it out of React's way. */}
      <div className="session-terminal-mount" ref={containerRef} />
      {menu ? (
        <TerminalContextMenu
          x={menu.x}
          y={menu.y}
          entries={menuEntries}
          onClose={closeMenu}
        />
      ) : null}
    </div>
  );
}

export default forwardRef(SessionTerminal);
