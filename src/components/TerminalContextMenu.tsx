/**
 * The terminal's right-click menu (spec section 11).
 *
 * The webview's own context menu is never shown for the terminal - the caller
 * calls `preventDefault()` before opening this one - because it would offer
 * "Reload" / "Inspect" against a canvas that has no DOM text, while the actions
 * a terminal actually needs (copy, paste, scroll) live in xterm's buffer.
 *
 * Presentational on purpose: the entries and their enabled state are decided by
 * whoever owns the terminal handle, so this component only has to be a
 * correctly-positioned, correctly-closed menu with the app's `.menu` styling.
 */

import { useEffect, useLayoutEffect, useRef, useState } from "react";

/** One row of the menu. */
export interface ContextMenuEntry {
  /** Stable id, also used as the `data-menu-entry` test hook. */
  id: string;
  label: string;
  /** Right-aligned shortcut hint, e.g. `Ctrl+C`. */
  hint?: string;
  /** Greyed out and unclickable (e.g. "Copy" with nothing selected). */
  disabled?: boolean;
  /**
   * Rows with a different group than the previous one are separated by a rule,
   * which keeps the grouping a property of the data rather than of the JSX.
   */
  group: number;
  run(): void;
}

interface Props {
  /** Pointer position in viewport coordinates (an `onContextMenu` event). */
  x: number;
  y: number;
  entries: ContextMenuEntry[];
  /** Called for Escape, an outside pointer press, a scroll and after a run. */
  onClose(): void;
}

/** Keeps a menu opened at the pointer edge inside the window. */
const VIEWPORT_MARGIN = 4;

/**
 * A small dark context menu, positioned at the pointer.
 *
 * Closed by: Escape, a pointer press outside, any scroll (including the
 * terminal's own, so it cannot float over text that moved), and after an entry
 * runs. It never traps focus: the terminal keeps it, so the user can go
 * straight back to typing.
 */
export default function TerminalContextMenu({ x, y, entries, onClose }: Props) {
  const rootRef = useRef<HTMLDivElement | null>(null);

  // Clamped against the real menu size in a layout effect; in a test (jsdom has
  // no layout) the measurement is 0 and the position is simply the pointer's.
  const [position, setPosition] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const element = rootRef.current;
    if (!element) {
      return;
    }
    const rect = element.getBoundingClientRect();
    setPosition({
      left: Math.max(
        VIEWPORT_MARGIN,
        Math.min(x, window.innerWidth - rect.width - VIEWPORT_MARGIN),
      ),
      top: Math.max(
        VIEWPORT_MARGIN,
        Math.min(y, window.innerHeight - rect.height - VIEWPORT_MARGIN),
      ),
    });
  }, [x, y]);

  // `onClose` is stable in practice (a `useState` setter wrapper), but the
  // listeners are re-bound if it ever is not, so a stale closure cannot keep a
  // dismissed menu alive.
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    function close(): void {
      closeRef.current();
    }
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        // The menu owns Escape while it is open; xterm must not also see it.
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    }
    function onPointerDown(event: PointerEvent): void {
      const root = rootRef.current;
      if (root !== null && event.target instanceof Node && root.contains(event.target)) {
        return;
      }
      close();
    }
    // Capture phase for all three: the terminal's viewport scrolls without
    // bubbling a scroll event to the window, and the menu must close when it
    // does - otherwise it would hang over text that has moved.
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", close, true);
    window.addEventListener("wheel", close, true);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("wheel", close, true);
      window.removeEventListener("blur", close);
    };
  }, []);

  return (
    <div
      ref={rootRef}
      className="menu context-menu"
      role="menu"
      aria-label="Terminal"
      style={{ left: `${position.left}px`, top: `${position.top}px` }}
    >
      <ul className="menu-list">
        {entries.map((entry, index) => (
          <li key={entry.id}>
            {index > 0 && entries[index - 1].group !== entry.group ? (
              <div className="menu-separator" role="separator" />
            ) : null}
            <button
              type="button"
              className="menu-item"
              role="menuitem"
              data-menu-entry={entry.id}
              disabled={entry.disabled === true}
              onClick={() => {
                // Closed first: the action may move focus or write to the
                // terminal, and a menu that outlives its own click reads as a
                // broken click.
                onClose();
                entry.run();
              }}
            >
              <span className="menu-item-title">{entry.label}</span>
              {entry.hint ? (
                <span className="menu-item-hint muted">{entry.hint}</span>
              ) : null}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
