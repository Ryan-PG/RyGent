import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

/**
 * jsdom does not implement `window.matchMedia`, which `@xterm/xterm` calls
 * while measuring its device pixel ratio (spec section 11 - the terminal is a
 * real xterm instance, never a stub). Without it every component that renders a
 * terminal throws on mount for a reason that has nothing to do with the code
 * under test. Tests answer `false` for every query, i.e. "no media feature
 * matches"; nothing in the app styles itself from a media query.
 */
if (typeof window !== "undefined" && typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
}

// Unmount anything a test rendered so DOM state never leaks between files.
afterEach(() => {
  cleanup();
});
