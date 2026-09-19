/**
 * Turning the stored theme preference into a palette on screen (spec section 12).
 *
 * `settings/theme.ts` has two halves and both are checked here: the resolution
 * (`mode` + OS preference -> `dark` | `light`) and the application (the
 * `data-theme` attribute the stylesheet keys off, plus the `localStorage` cache
 * that exists so the first frame after launch is already the right colour).
 *
 * jsdom does not implement `matchMedia`, and `test/setup.ts` polyfills it to
 * answer `false` for every query - which is why several of these tests read as
 * "light": that is the OS answer the harness gives. The tests that care install
 * a controllable fake instead.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  applyTheme,
  bootTheme,
  commitTheme,
  readCachedTheme,
  resolveTheme,
  systemPrefersDark,
  watchSystemTheme,
  writeCachedTheme,
} from "../settings/theme";
import { resetTauriMock, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

/** The attribute the stylesheet hangs both palettes off. */
function paintedTheme(): string | undefined {
  return document.documentElement.dataset.theme;
}

const CACHE_KEY = "ai-workspace.theme";

beforeEach(() => {
  resetTauriMock();
  localStorage.clear();
  delete document.documentElement.dataset.theme;
});

afterEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.theme;
});

/**
 * Install a `matchMedia` that reports `matches` and lets a test drive a change.
 *
 * @returns the objects needed to fire a change and put the real one back.
 */
function installMatchMedia(matches: boolean) {
  const listeners = new Set<(event: { matches: boolean }) => void>();
  const query = {
    matches,
    media: "(prefers-color-scheme: dark)",
    onchange: null,
    addEventListener: (_type: string, listener: (event: { matches: boolean }) => void) => {
      listeners.add(listener);
    },
    removeEventListener: (
      _type: string,
      listener: (event: { matches: boolean }) => void,
    ) => {
      listeners.delete(listener);
    },
    // The deprecated pair, which older WebView2 builds use instead.
    addListener: (listener: (event: { matches: boolean }) => void) => {
      listeners.add(listener);
    },
    removeListener: (listener: (event: { matches: boolean }) => void) => {
      listeners.delete(listener);
    },
    dispatchEvent: () => false,
  } as unknown as MediaQueryList;

  const original = window.matchMedia;
  window.matchMedia = () => query;
  return {
    fire(next: boolean): void {
      (query as { matches: boolean }).matches = next;
      for (const listener of listeners) {
        listener({ matches: next });
      }
    },
    listenerCount: () => listeners.size,
    restore(): void {
      window.matchMedia = original;
    },
  };
}

describe("resolveTheme", () => {
  it("returns an explicit mode unchanged, whatever the OS says", () => {
    expect(resolveTheme("dark", false)).toBe("dark");
    expect(resolveTheme("dark", true)).toBe("dark");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("light", false)).toBe("light");
  });

  it("follows the OS while the mode is system", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
  });
});

describe("systemPrefersDark", () => {
  it("reports the media query's answer", () => {
    const media = installMatchMedia(true);
    try {
      expect(systemPrefersDark()).toBe(true);
      media.fire(false);
      expect(systemPrefersDark()).toBe(false);
    } finally {
      media.restore();
    }
  });

  it("answers dark when the query cannot be evaluated at all", () => {
    // The app was dark-only before themes existed, so an environment that cannot
    // answer keeps the palette it has always had rather than flipping to a light
    // theme nobody asked for.
    const original = window.matchMedia;
    // @ts-expect-error - removing an API jsdom does not implement anyway.
    delete window.matchMedia;
    try {
      expect(systemPrefersDark()).toBe(true);
    } finally {
      window.matchMedia = original;
    }
  });
});

describe("applyTheme", () => {
  it("paints by setting the attribute the stylesheet keys off", () => {
    tauriRuntime.available = false;

    applyTheme("light");
    expect(paintedTheme()).toBe("light");

    applyTheme("dark");
    expect(paintedTheme()).toBe("dark");
  });

  it("applies the CSS half even when the native window cannot be themed", () => {
    // The titlebar sync needs a window and a capability; either can be missing.
    // Failing it must never surface as an error or block the page from repainting.
    tauriRuntime.available = true;

    applyTheme("light");

    expect(paintedTheme()).toBe("light");
  });
});

describe("commitTheme", () => {
  it("resolves, paints and caches in one step", () => {
    const resolved = commitTheme("light");

    expect(resolved).toBe("light");
    expect(paintedTheme()).toBe("light");
    expect(readCachedTheme()).toBe("light");
  });

  it("resolves system against the OS rather than storing the word", () => {
    const media = installMatchMedia(true);
    try {
      // The cache holds the *palette*, so the boot path never has to evaluate a
      // media query to know what to paint.
      expect(commitTheme("system")).toBe("dark");
      expect(readCachedTheme()).toBe("dark");
    } finally {
      media.restore();
    }
  });
});

describe("the boot-time cache", () => {
  it("round-trips a palette", () => {
    writeCachedTheme("light");
    expect(readCachedTheme()).toBe("light");
    expect(localStorage.getItem(CACHE_KEY)).toBe("light");
  });

  it("ignores anything that is not a palette", () => {
    localStorage.setItem(CACHE_KEY, "purple");
    expect(readCachedTheme()).toBeNull();

    localStorage.setItem(CACHE_KEY, "");
    expect(readCachedTheme()).toBeNull();
  });

  it("boots on the cached palette when there is one", () => {
    writeCachedTheme("light");
    // Whatever the OS currently says, the cached value is what the last run
    // painted - and re-deriving it would show the wrong colour for one frame.
    expect(bootTheme()).toBe("light");

    writeCachedTheme("dark");
    expect(bootTheme()).toBe("dark");
  });

  it("boots on the OS preference on a first launch", () => {
    const media = installMatchMedia(true);
    try {
      expect(bootTheme()).toBe("dark");
    } finally {
      media.restore();
    }
  });

  it("survives storage that refuses to be read", () => {
    const getItem = Storage.prototype.getItem;
    Storage.prototype.getItem = () => {
      throw new Error("storage is blocked");
    };
    try {
      // A blocked storage costs one frame of the default palette, nothing more.
      expect(readCachedTheme()).toBeNull();
    } finally {
      Storage.prototype.getItem = getItem;
    }
  });
});

describe("watchSystemTheme", () => {
  it("reports OS changes and stops when unsubscribed", () => {
    const media = installMatchMedia(false);
    try {
      const onChange = vi.fn();
      const unsubscribe = watchSystemTheme(onChange);
      expect(media.listenerCount()).toBe(1);

      media.fire(true);
      expect(onChange).toHaveBeenCalledTimes(1);

      unsubscribe();
      expect(media.listenerCount()).toBe(0);
      media.fire(false);
      expect(onChange).toHaveBeenCalledTimes(1);
    } finally {
      media.restore();
    }
  });

  it("is a no-op that can still be unsubscribed where there is no query", () => {
    const original = window.matchMedia;
    // @ts-expect-error - removing an API jsdom does not implement anyway.
    delete window.matchMedia;
    try {
      const unsubscribe = watchSystemTheme(() => {});
      expect(() => unsubscribe()).not.toThrow();
    } finally {
      window.matchMedia = original;
    }
  });
});
