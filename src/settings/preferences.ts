/**
 * Every UI preference the Settings tab can read or write (spec sections 12, 14).
 *
 * This module is the **only** place a preference value is interpreted. The
 * backend stores preferences as opaque strings and never parses them
 * (`get_ui_preference` in `src-tauri/src/persistence/mod.rs`), so the key names,
 * the defaults, the valid ranges and every conversion live here. A component
 * asks for `preferences.terminalFontSize.get(...)` and gets a number back - it
 * never sees the string, and never invents its own default.
 *
 * Two rules keep this honest:
 *
 * - **A malformed value reads as the default, never as an error.** A value
 *   written by a newer build (or hand-edited into the database) must not be able
 *   to break the Settings tab or the terminal. `parse` returning `null` is that
 *   fallback path, and the panel simply shows the default.
 * - **The defaults reproduce the behaviour the app had before this module
 *   existed**, so installing this build changes nothing for an existing user
 *   except where deliberately noted below.
 *
 * `ALLOWED_KEYS` in `src-tauri/src/commands/settings.rs` is the other half of
 * this contract: a key added here that is not in that list is rejected by the
 * backend, and the Rust suite asserts the two agree on the keys it knows.
 */

import type { ThemeMode, UiPreferences } from "../types";

/**
 * Turn one stored string into a typed value, falling back when it is absent or
 * unusable.
 *
 * Returning `null` rather than throwing is deliberate: a preference that cannot
 * be parsed falls back to its default instead of breaking the panel.
 */
export type PreferenceParser<T> = (raw: string) => T | null;

/** Serialize a typed value back to the string the backend stores. */
export type PreferenceSerializer<T> = (value: T) => string;

/**
 * One preference: its backend key, its default, and how to move between the
 * typed value and the stored string.
 */
export interface Preference<T> {
  /** Backend key, exactly as it appears in the Rust `ALLOWED_KEYS` list. */
  readonly key: string;
  /** Used when the key is unset, or when the stored value cannot be parsed. */
  readonly defaultValue: T;
  /** Parse a stored value; `null` means "unusable, use the default". */
  readonly parse: PreferenceParser<T>;
  /** Render a value for storage. */
  readonly serialize: PreferenceSerializer<T>;
  /** Read this preference out of a loaded preference map. */
  get(preferences: UiPreferences): T;
}

/** A preference with no writable range (booleans, enums, free-form ids). */
function makePreference<T>(
  key: string,
  defaultValue: T,
  parse: PreferenceParser<T>,
  serialize: PreferenceSerializer<T>,
): Preference<T> {
  return {
    key,
    defaultValue,
    parse,
    serialize,
    get: (preferences) => {
      const raw = preferences[key];
      if (raw === undefined) {
        return defaultValue;
      }
      return parse(raw) ?? defaultValue;
    },
  };
}

/** A preference whose value is a whole number inside a closed range. */
export interface BoundedPreference extends Preference<number> {
  readonly min: number;
  readonly max: number;
  /**
   * Force a number into the preference's range.
   *
   * Called before writing, so a value typed into the panel's number input (or
   * produced by a stale one) can never put the terminal into a state the range
   * forbids - a zero font size, for instance, which would leave no text at all.
   */
  clamp(value: number): number;
}

function makeBoundedPreference(
  key: string,
  defaultValue: number,
  min: number,
  max: number,
): BoundedPreference {
  const parse = (raw: string): number | null => {
    // `Number` maps "" and whitespace to 0, and 0 is out of range for both of
    // these preferences, so an empty string is already rejected below - but the
    // trim keeps the intent explicit rather than relying on that.
    if (raw.trim() === "") {
      return null;
    }
    const value = Number(raw);
    if (!Number.isInteger(value) || value < min || value > max) {
      return null;
    }
    return value;
  };
  const clamp = (value: number): number => {
    if (!Number.isFinite(value)) {
      return defaultValue;
    }
    return Math.min(max, Math.max(min, Math.round(value)));
  };
  return {
    key,
    defaultValue,
    min,
    max,
    parse,
    clamp,
    serialize: (value) => String(clamp(value)),
    get: (preferences) => {
      const raw = preferences[key];
      if (raw === undefined) {
        return defaultValue;
      }
      return parse(raw) ?? defaultValue;
    },
  };
}

/** A preference stored as the literal strings `"true"` / `"false"`. */
function makeBooleanPreference(
  key: string,
  defaultValue: boolean,
): Preference<boolean> {
  return makePreference(
    key,
    defaultValue,
    (raw) => {
      if (raw === "true") {
        return true;
      }
      if (raw === "false") {
        return false;
      }
      return null;
    },
    (value) => (value ? "true" : "false"),
  );
}

const THEME_MODES: readonly ThemeMode[] = ["dark", "light", "system"];

export const preferences = {
  /**
   * The palette to paint with.
   *
   * Defaults to `system`: a fresh install matches the machine it is on rather
   * than imposing a palette, which is the behaviour a desktop app is expected to
   * have. Note this is the one default that differs from "what the app did
   * before", since the app was previously dark unconditionally - but a *system*
   * default on a dark machine still resolves to dark, so an existing dark-mode
   * user sees no change at all.
   */
  appearanceTheme: makePreference<ThemeMode>(
    "appearance.theme",
    "system",
    (raw) =>
      (THEME_MODES as readonly string[]).includes(raw) ? (raw as ThemeMode) : null,
    (value) => value,
  ),

  /** Terminal font size in px. */
  terminalFontSize: makeBoundedPreference("terminal.fontSize", 12, 8, 24),

  /**
   * Lines of scrollback the terminal keeps.
   *
   * The same default the terminal shipped with (`TERMINAL_SCROLLBACK` in
   * `SessionTerminal.tsx`), so widening this setting changes nothing until it is
   * actually used.
   */
  terminalScrollback: makeBoundedPreference(
    "terminal.scrollback",
    10_000,
    1_000,
    50_000,
  ),

  terminalCursorBlink: makeBooleanPreference("terminal.cursorBlink", true),

  /**
   * Copy the selection as soon as it is made.
   *
   * Off by default: selecting text to read it is far more common than selecting
   * it to copy, and the terminal already offers Ctrl+C, Ctrl+Shift+C and a
   * right-click menu.
   */
  terminalCopyOnSelect: makeBooleanPreference("terminal.copyOnSelect", false),

  /** Reopen the tabs that were open when the app last closed. */
  sessionsRestoreTabs: makeBooleanPreference("sessions.restoreTabs", true),

  /**
   * Ask before closing a tab whose agent is still running.
   *
   * On by default. This is the one preference whose default *adds* a step the
   * app did not have before: closing a tab stops its session, and an agent
   * mid-task is the kind of thing a user does not want discarded by a misclick.
   */
  sessionsConfirmCloseRunning: makeBooleanPreference(
    "sessions.confirmCloseRunning",
    true,
  ),

  /**
   * Provider preselected in the New Workspace dialog.
   *
   * An empty string means "no preference - use the first configured profile",
   * which is exactly what the dialog did before this setting existed. The value
   * is a provider *id*, never a credential.
   */
  workspacesDefaultProvider: makePreference<string>(
    "workspaces.defaultProviderId",
    "",
    (raw) => raw,
    (value) => value,
  ),
} as const;

/** Every preference, for the panel to render and for tests to enumerate. */
export const ALL_PREFERENCES = [
  preferences.appearanceTheme,
  preferences.terminalFontSize,
  preferences.terminalScrollback,
  preferences.terminalCursorBlink,
  preferences.terminalCopyOnSelect,
  preferences.sessionsRestoreTabs,
  preferences.sessionsConfirmCloseRunning,
  preferences.workspacesDefaultProvider,
] as const;

/**
 * Human-readable byte count for the About section.
 *
 * Uses binary units (KiB/MiB/GiB) because that is what a file manager on the
 * supported platforms reports, so the number here matches what the user sees
 * when they open the data folder.
 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // One decimal below 10 (so 1.5 MiB), none above (so 128 KiB) - a rounded
  // integer of a large number is already precise enough to read.
  const rounded = value < 10 ? value.toFixed(1) : Math.round(value).toString();
  return `${rounded} ${units[unit]}`;
}
