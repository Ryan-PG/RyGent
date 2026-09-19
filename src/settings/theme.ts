/**
 * Turning the stored theme preference into a palette actually on screen
 * (spec section 12, "Appearance").
 *
 * The app paints itself from CSS custom properties, so applying a theme is one
 * attribute on `<html>`: `styles.css` defines the light tokens on `:root` and
 * overrides them under `:root[data-theme="dark"]`. The xterm theme is the
 * separate half and is built in `SessionTerminal.tsx` from the resolved value.
 *
 * ## Why a cached copy lives in `localStorage`
 *
 * The stored preference is read from SQLite over an async command, so on the
 * first frame after launch the app does not yet know which palette to use. A
 * light-theme user would see a dark flash on every start. The resolved theme is
 * therefore mirrored into `localStorage` and re-applied *before React renders*
 * (see `main.tsx`), which is fast enough to beat the first paint.
 *
 * SQLite remains the source of truth - the cache is only ever read at boot and
 * overwritten whenever the real preference loads.
 */

import type { ResolvedTheme, ThemeMode } from "../types";
import { isBackendAvailable } from "../services/backend";

/** `localStorage` key for the boot-time theme cache. */
const THEME_CACHE_KEY = "ai-workspace.theme";

/** The media query that reports the OS palette. */
const DARK_MEDIA_QUERY = "(prefers-color-scheme: dark)";

/**
 * Resolve the stored mode into the palette to paint.
 *
 * `system` follows `prefersDark`. An unrecognised mode cannot reach here - the
 * preference parser rejects it and yields the default - so this is total over
 * `ThemeMode`.
 */
export function resolveTheme(mode: ThemeMode, prefersDark: boolean): ResolvedTheme {
  if (mode === "system") {
    return prefersDark ? "dark" : "light";
  }
  return mode;
}

/**
 * Whether the OS currently prefers a dark palette.
 *
 * Defaults to dark when the query cannot be evaluated. The app was dark-only
 * before themes existed, so an environment that cannot answer (an old webview,
 * or jsdom) keeps the palette it has always had rather than flipping to a light
 * theme nobody asked for.
 */
export function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return true;
  }
  try {
    return window.matchMedia(DARK_MEDIA_QUERY).matches;
  } catch {
    return true;
  }
}

/**
 * Subscribe to OS palette changes.
 *
 * Only meaningful while the mode is `system`, but the subscription itself is
 * unconditional: re-subscribing on every mode change would be more moving parts
 * than re-resolving a value that is ignored when the mode is explicit.
 *
 * @returns an unsubscribe function.
 */
export function watchSystemTheme(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const query = window.matchMedia(DARK_MEDIA_QUERY);
  // `addEventListener` is the modern form; older WebView2 builds only have the
  // deprecated `addListener`. Both are checked so a missing one cannot throw.
  if (typeof query.addEventListener === "function") {
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }
  if (typeof query.addListener === "function") {
    query.addListener(onChange);
    return () => query.removeListener(onChange);
  }
  return () => {};
}

/**
 * Paint a resolved theme.
 *
 * Also syncs the native window theme, so the titlebar and the webview's own
 * default form-control and scrollbar colours match the page. That call needs
 * the `core:window:allow-set-theme` capability and is best effort: without it -
 * or outside Tauri entirely - the CSS half still applies, which is the half that
 * is actually visible.
 */
export function applyTheme(theme: ResolvedTheme): void {
  if (typeof document === "undefined") {
    return;
  }
  document.documentElement.dataset.theme = theme;

  if (!isBackendAvailable()) {
    return;
  }
  // Fire-and-forget, and deliberately silent: failing to restyle the titlebar
  // must never surface as an error or block the theme change.
  void (async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().setTheme(theme);
    } catch {
      // No window, no permission, or a webview without the API - the page is
      // already themed by the line above.
    }
  })();
}

/** Read the boot-time theme cache; `null` when it was never written. */
export function readCachedTheme(): ResolvedTheme | null {
  if (typeof localStorage === "undefined") {
    return null;
  }
  try {
    const cached = localStorage.getItem(THEME_CACHE_KEY);
    return cached === "dark" || cached === "light" ? cached : null;
  } catch {
    // Blocked storage (a privacy setting, an embedded context) is not an error:
    // the app just gets the default palette for one frame.
    return null;
  }
}

/**
 * Remember the resolved theme for the next launch.
 *
 * Stores the **resolved** palette rather than the mode, because the boot path
 * cannot answer "what would `system` mean right now" any more cheaply than the
 * media query it would have to evaluate anyway - and a stale explicit palette is
 * corrected on the next frame regardless.
 */
export function writeCachedTheme(theme: ResolvedTheme): void {
  if (typeof localStorage === "undefined") {
    return;
  }
  try {
    localStorage.setItem(THEME_CACHE_KEY, theme);
  } catch {
    // Same as above: a cache that cannot be written costs one flash, nothing
    // more. This must never break a theme change.
  }
}

/**
 * Resolve, apply and cache in one step - the whole of "the theme changed".
 *
 * Used by both the boot path and the store, so the two can never disagree about
 * what a theme change involves.
 */
export function commitTheme(mode: ThemeMode): ResolvedTheme {
  const theme = resolveTheme(mode, systemPrefersDark());
  applyTheme(theme);
  writeCachedTheme(theme);
  return theme;
}

/**
 * The palette to paint before the stored preference has been read.
 *
 * Two callers need the same answer and must not compute it differently: the
 * pre-render line in `main.tsx`, and the store's initial `resolvedTheme` (which
 * `SessionView` feeds to the terminal). If they disagreed, the terminal would
 * paint one palette inside a window painted in the other.
 */
export function bootTheme(): ResolvedTheme {
  // No cache means a first launch, where the OS is the best available guess -
  // and the same guess the `system` default will make a moment later, so the
  // real preference normally lands without any visible change.
  return readCachedTheme() ?? resolveTheme("system", systemPrefersDark());
}
