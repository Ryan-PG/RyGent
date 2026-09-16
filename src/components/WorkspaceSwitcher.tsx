/**
 * The "open workspace" list (spec section 10: "Reopen configured workspaces").
 *
 * Every configured workspace is listed, with the ones that already have a tab
 * marked: clicking an open one activates its tab, clicking a closed one reopens
 * it. No session is started by reopening - the tab comes back stopped, exactly
 * like after a restart (spec section 14).
 *
 * The list is portaled to `<body>` and positioned in viewport coordinates
 * (`position: fixed` plus an inline `left`/`top` pair), for the same reason the
 * terminal context menu is: it opens *below* the tab strip, and `.tabbar-tabs`
 * scrolls horizontally (`overflow-x: auto`), which makes CSS compute
 * `overflow-y: auto` as well. Anything positioned inside that subtree is clipped
 * the moment it leaves the strip's box - the menu rendered, and nothing was
 * visible or clickable. A body-level portal cannot be clipped by that ancestor
 * (or by a future transform/stacking context), and the `fixed` positioning it
 * needs is inline rather than left to a stylesheet rule, so no cascade surprise
 * can drop the menu back inside the strip.
 */

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { useAppStore } from "../stores/useAppStore";

interface Props {
  /**
   * The "Open ▾" button this list hangs from. Its viewport rect decides where
   * the menu goes, and it counts as *inside* for the click-outside check: that
   * button toggles this menu, so its own press must not be read as "clicked
   * away" - closing and immediately reopening on one click.
   */
  anchorRef: RefObject<HTMLElement | null>;
  onClose: () => void;
}

/** Distance between the trigger and the menu, in px. */
const GAP = 4;
/** Keep at least this much of the window between the menu and each edge. */
const VIEWPORT_MARGIN = 4;

/** The geometry of an element, in viewport coordinates. */
interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

/** Stands in for a trigger that is not mounted: the window's top-left corner. */
const NO_ANCHOR: Rect = {
  left: 0,
  top: 0,
  right: 0,
  bottom: 0,
  width: 0,
  height: 0,
};

interface Position {
  left: number;
  top: number;
}

/** Clamp into `[VIEWPORT_MARGIN, limit]`, preferring the margin if the two cross. */
function clamp(value: number, limit: number): number {
  return Math.min(
    Math.max(value, VIEWPORT_MARGIN),
    Math.max(VIEWPORT_MARGIN, limit),
  );
}

/**
 * Where the menu goes, in viewport coordinates.
 *
 * Under the trigger by default, right-aligned to it when the menu would run off
 * the right edge, flipped above it when the space below cannot hold the menu,
 * and clamped on both axes so it is never partly off-screen. A menu larger than
 * the window - impossible with the stylesheet's `max-height` - sits at the
 * margin rather than at a negative coordinate.
 */
function placeMenu(
  anchor: Rect,
  menu: Rect,
  viewport: { width: number; height: number },
): Position {
  let left = anchor.left;
  if (left + menu.width > viewport.width - VIEWPORT_MARGIN) {
    left = anchor.right - menu.width;
  }

  let top = anchor.bottom + GAP;
  const above = anchor.top - GAP - menu.height;
  if (
    top + menu.height > viewport.height - VIEWPORT_MARGIN &&
    above >= VIEWPORT_MARGIN
  ) {
    top = above;
  }

  return {
    left: clamp(left, viewport.width - menu.width - VIEWPORT_MARGIN),
    top: clamp(top, viewport.height - menu.height - VIEWPORT_MARGIN),
  };
}

export default function WorkspaceSwitcher({ anchorRef, onClose }: Props) {
  const workspaces = useAppStore((s) => s.workspaces);
  const tabs = useAppStore((s) => s.tabs);
  const workspacesLoaded = useAppStore((s) => s.workspacesLoaded);
  const openWorkspace = useAppStore((s) => s.openWorkspace);
  const rootRef = useRef<HTMLDivElement | null>(null);

  // Null until the menu has been measured: its position depends on its own
  // size, which only exists after the first commit. The layout effect runs
  // before the browser paints, so the hidden placeholder is never seen.
  const [position, setPosition] = useState<Position | null>(null);
  useLayoutEffect(() => {
    const menu = rootRef.current;
    if (menu === null) {
      return;
    }
    const anchor = anchorRef.current;
    setPosition(
      placeMenu(
        anchor === null ? NO_ANCHOR : anchor.getBoundingClientRect(),
        menu.getBoundingClientRect(),
        { width: window.innerWidth, height: window.innerHeight },
      ),
    );
    // Re-measured when the *contents* change: the list is what gives the menu
    // its size, and the placement is derived from that size (the loading note
    // versus a 320 px-capped list is a different decision near an edge).
  }, [anchorRef, workspacesLoaded, workspaces, tabs]);

  // Escape and a press outside both close the popover, like a menu.
  //
  // "Outside" has to mean the portal *and* the trigger, because the popover is
  // no longer a descendant of the button's container: checking only the menu
  // would treat a press on the trigger as a click-away, closing the menu on
  // mousedown and letting the button's own click reopen it - so the trigger
  // could never close its own menu.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }
      if (rootRef.current?.contains(target) === true) {
        return;
      }
      if (anchorRef.current?.contains(target) === true) {
        return;
      }
      onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("mousedown", onMouseDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("mousedown", onMouseDown);
    };
  }, [anchorRef, onClose]);

  // The position is computed once, so anything that moves the trigger under the
  // menu leaves it pointing at empty space; the tab strip scrolls horizontally
  // and the window can be resized. Closing is less surprising than tracking.
  // A scroll *inside* the menu is exempt - a long workspace list scrolls by
  // design (`max-height` + `overflow: auto`).
  useEffect(() => {
    const onScroll = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && rootRef.current?.contains(target) === true) {
        return;
      }
      onClose();
    };
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);

  const style: CSSProperties =
    position === null
      ? // Measured before the first paint; `visibility` (not `display`) keeps
        // the menu measurable without ever flashing at the window's corner.
        { position: "fixed", left: 0, top: 0, visibility: "hidden" }
      : {
          position: "fixed",
          left: `${position.left}px`,
          top: `${position.top}px`,
        };

  return createPortal(
    <div
      className="menu workspace-switcher"
      ref={rootRef}
      role="menu"
      aria-label="Configured workspaces"
      style={style}
    >
      {!workspacesLoaded ? (
        <p className="menu-note muted">Loading workspaces…</p>
      ) : workspaces.length === 0 ? (
        <p className="menu-note muted">No workspaces configured yet.</p>
      ) : (
        <ul className="menu-list">
          {workspaces.map((workspace) => {
            const open = tabs.some((tab) => tab.id === workspace.id);
            return (
              <li key={workspace.id}>
                <button
                  type="button"
                  role="menuitem"
                  className="menu-item"
                  onClick={() => {
                    openWorkspace(workspace.id);
                    onClose();
                  }}
                  title={workspace.projectPath}
                >
                  <span className="menu-item-title">
                    <span
                      className={"status-dot " + (open ? "dot-running" : "dot-idle")}
                      aria-hidden="true"
                    />
                    {workspace.name}
                  </span>
                  <span className="menu-item-hint muted">
                    {open ? "open" : "reopen"}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>,
    document.body,
  );
}
