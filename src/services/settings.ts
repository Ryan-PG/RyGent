/**
 * Typed wrappers over the settings commands (spec sections 12, 14, 16).
 *
 * Command names and argument shapes mirror
 * `src-tauri/src/commands/settings.rs`. Nothing here carries a credential:
 * the Settings tab manages palettes, terminal geometry and session behaviour,
 * and every value it writes is a plain, non-secret string (spec section 17).
 *
 * Preferences are read and written one key at a time. `setUiPreference` with an
 * **empty value deletes** the row, which is how the panel's reset-to-default
 * works - "unset" and "set to empty" stay indistinguishable, and the frontend
 * default applies again (see `settings/preferences.ts`).
 */

import { call } from "./backend";
import type { AppInfo, UiPreferences } from "../types";

export const settingsApi = {
  /** Every stored preference. Loaded once at startup. */
  list: (): Promise<UiPreferences> =>
    call<UiPreferences>("list_ui_preferences"),

  /** Store one preference. An empty `value` resets it to its default. */
  set: (key: string, value: string): Promise<void> =>
    call<void>("set_ui_preference", { key, value }),

  /** Version, paths, database size and schema version for the About section. */
  appInfo: (): Promise<AppInfo> => call<AppInfo>("app_info"),

  /** Open the application data directory in the OS file manager. */
  openDataDirectory: (): Promise<void> => call<void>("open_data_directory"),
};
