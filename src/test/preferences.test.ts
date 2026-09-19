/**
 * The preference module: keys, defaults, parsing and ranges (spec sections 12, 14).
 *
 * This is the frontend half of the preference contract. The backend stores
 * opaque strings and never parses them, so *everything* about what a value means
 * lives here - and the two rules that make that safe are what most of these
 * tests pin:
 *
 * - an unset or unparseable value reads as the default, never as an error;
 * - the defaults reproduce the behaviour the app had before this module existed.
 *
 * `ALLOWED_KEYS` in `src-tauri/src/commands/settings.rs` is the other half, and
 * the Rust suite asserts the key list it knows. The test below pins the exact
 * strings so a rename on one side cannot pass unnoticed on the other.
 */

import { describe, expect, it } from "vitest";
import {
  ALL_PREFERENCES,
  formatBytes,
  preferences,
  type Preference,
} from "../settings/preferences";
import type { UiPreferences } from "../types";

/** The stored map a write of `value` through `preference` would produce. */
function roundTrip<T>(preference: Preference<T>, value: T): T {
  return preference.get({ [preference.key]: preference.serialize(value) });
}

describe("preferences: keys and defaults", () => {
  it("uses exactly the keys the Rust allowlist permits", () => {
    expect(ALL_PREFERENCES.map((preference) => preference.key)).toEqual([
      "appearance.theme",
      "terminal.fontSize",
      "terminal.scrollback",
      "terminal.cursorBlink",
      "terminal.copyOnSelect",
      "sessions.restoreTabs",
      "sessions.confirmCloseRunning",
      "workspaces.defaultProviderId",
    ]);
    // One descriptor per key: duplicate keys would make the panel render two
    // rows that write the same row.
    const keys = ALL_PREFERENCES.map((preference) => preference.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("reads every default out of an empty map", () => {
    const empty: UiPreferences = {};

    expect(preferences.appearanceTheme.get(empty)).toBe("system");
    expect(preferences.terminalFontSize.get(empty)).toBe(12);
    expect(preferences.terminalScrollback.get(empty)).toBe(10_000);
    expect(preferences.terminalCursorBlink.get(empty)).toBe(true);
    // Selecting text to read it is more common than selecting it to copy, and
    // the terminal already offers Ctrl+C and a right-click menu.
    expect(preferences.terminalCopyOnSelect.get(empty)).toBe(false);
    expect(preferences.sessionsRestoreTabs.get(empty)).toBe(true);
    // The one default that *adds* a step the app did not have before.
    expect(preferences.sessionsConfirmCloseRunning.get(empty)).toBe(true);
    expect(preferences.workspacesDefaultProvider.get(empty)).toBe("");
  });
});

describe("preferences: a value that cannot be used is not an error", () => {
  it("falls back to the default rather than throwing or yielding garbage", () => {
    // Every one of these could plausibly be written by a newer build or by hand
    // in the database. None of them may break the panel.
    const stored: UiPreferences = {
      "appearance.theme": "solarized-midnight",
      "terminal.fontSize": "huge",
      "terminal.scrollback": "-1",
      "terminal.cursorBlink": "yes",
      "sessions.restoreTabs": "TRUE",
    };

    expect(preferences.appearanceTheme.get(stored)).toBe("system");
    expect(preferences.terminalFontSize.get(stored)).toBe(12);
    expect(preferences.terminalScrollback.get(stored)).toBe(10_000);
    expect(preferences.terminalCursorBlink.get(stored)).toBe(true);
    expect(preferences.sessionsRestoreTabs.get(stored)).toBe(true);
  });

  it("treats an out-of-range number as unusable on read, but clamps it on write", () => {
    // The asymmetry is deliberate: reading is not allowed to invent a value the
    // user never chose, while writing must land inside the range the terminal
    // can survive - a font size of 0 would leave no text at all.
    expect(preferences.terminalFontSize.get({ "terminal.fontSize": "500" })).toBe(
      12,
    );
    expect(preferences.terminalFontSize.get({ "terminal.fontSize": "0" })).toBe(
      12,
    );
    expect(preferences.terminalFontSize.get({ "terminal.fontSize": "12.5" })).toBe(
      12,
    );
    expect(preferences.terminalFontSize.get({ "terminal.fontSize": "" })).toBe(
      12,
    );
    expect(preferences.terminalFontSize.clamp(500)).toBe(24);
    expect(preferences.terminalFontSize.clamp(0)).toBe(8);
  });

  it("rejects a boolean that is spelled any other way", () => {
    expect(
      preferences.terminalCursorBlink.get({ "terminal.cursorBlink": "1" }),
    ).toBe(true);
    expect(
      preferences.terminalCursorBlink.get({ "terminal.cursorBlink": "false" }),
    ).toBe(false);
    expect(
      preferences.terminalCursorBlink.get({ "terminal.cursorBlink": "" }),
    ).toBe(true);
  });
});

describe("preferences: writing then reading", () => {
  it("round-trips every preference through its own serializer", () => {
    // A serialize/parse pair that disagreed would make a setting appear to
    // revert the moment the app was restarted.
    expect(roundTrip(preferences.appearanceTheme, "light")).toBe("light");
    expect(roundTrip(preferences.appearanceTheme, "dark")).toBe("dark");
    expect(roundTrip(preferences.appearanceTheme, "system")).toBe("system");
    expect(roundTrip(preferences.terminalFontSize, 18)).toBe(18);
    expect(roundTrip(preferences.terminalScrollback, 50_000)).toBe(50_000);
    expect(roundTrip(preferences.terminalCursorBlink, false)).toBe(false);
    expect(roundTrip(preferences.terminalCopyOnSelect, true)).toBe(true);
    expect(roundTrip(preferences.sessionsRestoreTabs, false)).toBe(false);
    expect(roundTrip(preferences.workspacesDefaultProvider, "prov-2")).toBe(
      "prov-2",
    );
  });

  it("clamps and rounds a bounded value as it serializes it", () => {
    // The panel is not the only writer: whatever writes a number gets a value
    // inside the range, so the terminal can never be handed a broken one.
    expect(preferences.terminalFontSize.serialize(900)).toBe("24");
    expect(preferences.terminalFontSize.serialize(14.6)).toBe("15");
    expect(roundTrip(preferences.terminalFontSize, 900)).toBe(24);
  });
});

describe("formatBytes", () => {
  it("uses the binary units a file manager reports", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    // One decimal below 10, so a small size stays readable...
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(2_621_440)).toBe("2.5 MiB");
    // ...and none above it, where the extra digit is noise.
    expect(formatBytes(128 * 1024)).toBe("128 KiB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5.0 GiB");
  });
});
