/**
 * The Settings tab (spec sections 12, 14, 17).
 *
 * `SettingsPanel` is driven through real clicks; the store is the real store and
 * only the Tauri bridge is faked, so a click here exercises the whole path the
 * app uses: control -> `setPreference` -> `set_ui_preference` -> the optimistic
 * local update (and back again if the write fails).
 *
 * Two behaviours are asserted throughout rather than assumed:
 *
 * - **every control writes on change, and its Reset writes the empty value** that
 *   deletes the row, which is what makes "reset to default" work at all;
 * - **nothing here can carry a credential.** Every value written from this panel
 *   is a palette name, a number, a boolean or a provider **id** (section 17), and
 *   the tests below pin the exact strings to keep it that way.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import SettingsPanel from "../components/SettingsPanel";
import { preferences } from "../settings/preferences";
import type { AppInfo, ProviderProfile, UiPreferences } from "../types";
import {
  makeAgent,
  makeAgents,
  makeProvider,
  resetAppStore,
  seedPreferences,
  setupUser,
} from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

const PROVIDER_A = makeProvider();
const PROVIDER_B = makeProvider({
  id: "prov-2",
  name: "Provider B",
  baseUrl: "https://provider-b.example.com",
  model: "model-b",
});

/** What `app_info` returns in the running app, paths and all. */
const APP_INFO: AppInfo = {
  version: "0.3.1",
  dataDirectory: "C:\\Users\\ryan\\AppData\\Roaming\\com.rygent.app",
  databasePath: "C:\\Users\\ryan\\AppData\\Roaming\\com.rygent.app\\workspace.sqlite3",
  databaseSizeBytes: 2_621_440,
  agents: makeAgents(),
  schemaVersion: 4,
};

/**
 * Stub every command the panel issues.
 *
 * `app_info` is unconditional (the About section always loads) and the provider
 * list is loaded because the Workspaces section selects from it.
 */
function stubSettings(
  options: {
    preferences?: UiPreferences;
    appInfo?: AppInfo;
    providers?: ProviderProfile[];
  } = {},
): void {
  stubCommands({
    list_ui_preferences: () => options.preferences ?? {},
    set_ui_preference: () => null,
    app_info: () => options.appInfo ?? APP_INFO,
    list_providers: () => options.providers ?? [],
    provider_secret_status: () => false,
    open_data_directory: () => null,
  });
}

/** The row for one preference, found by the key it writes. */
function settingRow(key: string): HTMLElement {
  const row = document.querySelector(`[data-setting="${key}"]`);
  if (row === null) {
    throw new Error(`no settings row for "${key}"`);
  }
  return row as HTMLElement;
}

/** The Reset button belonging to one preference's row. */
function resetButton(key: string): HTMLElement {
  return within(settingRow(key)).getByRole("button", { name: "Reset" });
}

/** The arguments the last `set_ui_preference` call carried. */
function lastWrite(): { key: string; value: string } | undefined {
  const writes = invokeMock.mock.calls.filter(
    ([command]) => command === "set_ui_preference",
  );
  const last = writes[writes.length - 1];
  return last?.[1] as { key: string; value: string } | undefined;
}

/**
 * Render the panel and wait for its startup reads to land.
 *
 * The panel loads the stored preferences, the provider list and `app_info` on
 * mount, so anything asserted or clicked before those settle would be reading
 * the defaults - or, worse, a control that is still disabled because its options
 * have not arrived. The header reports the state of the preference load, which
 * makes it the signal to wait on.
 */
async function renderPanel(): Promise<void> {
  render(<SettingsPanel />);
  await screen.findByText("stored in the application database");
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

afterEach(() => {
  // The theme is applied to the real document, which outlives the test.
  delete document.documentElement.dataset.theme;
  localStorage.clear();
});

describe("SettingsPanel: the sections", () => {
  it("renders every section with the settings it owns", async () => {
    stubSettings();
    await renderPanel();

    for (const heading of [
      "Appearance",
      "Terminal",
      "Sessions",
      "Workspaces",
      "About / Storage",
    ]) {
      expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument();
    }

    expect(screen.getByRole("radiogroup", { name: "Theme" })).toBeInTheDocument();
    expect(screen.getByLabelText("Font size in pixels")).toBeInTheDocument();
    expect(screen.getByLabelText("Scrollback lines")).toBeInTheDocument();
    expect(screen.getByLabelText("Blinking cursor")).toBeInTheDocument();
    expect(screen.getByLabelText("Copy on select")).toBeInTheDocument();
    expect(screen.getByLabelText("Restore tabs at launch")).toBeInTheDocument();
    expect(
      screen.getByLabelText("Confirm before closing a running session"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Default provider")).toBeInTheDocument();
  });

  it("renders a stored value instead of the default", async () => {
    stubSettings({
      preferences: {
        "terminal.fontSize": "18",
        "terminal.copyOnSelect": "true",
        "sessions.confirmCloseRunning": "false",
      },
    });
    await renderPanel();

    expect(screen.getByLabelText("Font size in pixels")).toHaveValue(18);
    expect(screen.getByLabelText("Copy on select")).toBeChecked();
    expect(
      screen.getByLabelText("Confirm before closing a running session"),
    ).not.toBeChecked();
  });

  it("reads an unusable stored value as the default rather than failing", async () => {
    // A value written by a newer build must not be able to break this panel.
    stubSettings({ preferences: { "terminal.fontSize": "gigantic" } });
    await renderPanel();

    expect(screen.getByLabelText("Font size in pixels")).toHaveValue(12);
    // Still *stored*, so Reset is offered: the row can be cleared even though it
    // already reads as the default.
    expect(resetButton("terminal.fontSize")).toBeEnabled();
  });

  it("offers Reset as disabled while a row is already at its default", async () => {
    stubSettings();
    await renderPanel();

    expect(resetButton("terminal.fontSize")).toBeDisabled();

    // ...and enables it once the row holds something.
    seedPreferences({ "terminal.fontSize": "18" });
    await waitFor(() => expect(resetButton("terminal.fontSize")).toBeEnabled());
  });
});

describe("SettingsPanel: writing a setting", () => {
  it("saves the theme on change, applying it to the window immediately", async () => {
    const user = setupUser();
    stubSettings();
    await renderPanel();

    await user.click(screen.getByRole("radio", { name: "Light" }));

    expect(lastWrite()).toEqual({
      key: "appearance.theme",
      value: "light",
    });
    // The optimistic half: the palette is repainted without waiting for SQLite.
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("shows the stored theme as the selected choice", async () => {
    stubSettings({ preferences: { "appearance.theme": "dark" } });
    await renderPanel();

    expect(screen.getByRole("radio", { name: "Dark" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "System" })).not.toBeChecked();
  });

  it("writes a boolean as the strings the backend stores", async () => {
    const user = setupUser();
    stubSettings();
    await renderPanel();

    await user.click(screen.getByLabelText("Blinking cursor"));
    expect(lastWrite()).toEqual({
      key: "terminal.cursorBlink",
      value: "false",
    });

    await user.click(screen.getByLabelText("Restore tabs at launch"));
    expect(lastWrite()).toEqual({
      key: "sessions.restoreTabs",
      value: "false",
    });
  });

  it("writes a number on commit, not on every keystroke", async () => {
    const user = setupUser();
    stubSettings();
    await renderPanel();
    const field = screen.getByLabelText("Font size in pixels");

    await user.clear(field);
    await user.type(field, "1");
    // Mid-typing the field holds "1", which is below the minimum: a directly
    // controlled input would have clamped it to 8 and mangled the entry.
    expect(field).toHaveValue(1);
    expect(lastWrite()).toBeUndefined();

    await user.type(field, "8");
    await user.tab();

    expect(lastWrite()).toEqual({ key: "terminal.fontSize", value: "18" });
  });

  it("clamps a number outside the range instead of writing it through", async () => {
    const user = setupUser();
    stubSettings();
    await renderPanel();
    const field = screen.getByLabelText("Font size in pixels");

    await user.clear(field);
    await user.type(field, "900");
    await user.keyboard("{Enter}");

    // 24 is the maximum; a font size the terminal cannot render must never be
    // stored, however it was typed.
    expect(lastWrite()).toEqual({ key: "terminal.fontSize", value: "24" });
    expect(field).toHaveValue(24);
  });

  it("puts the stored value back when the field is left unusable", async () => {
    const user = setupUser();
    stubSettings({ preferences: { "terminal.fontSize": "18" } });
    await renderPanel();
    const field = screen.getByLabelText("Font size in pixels");

    await user.clear(field);
    await user.tab();

    expect(field).toHaveValue(18);
    expect(lastWrite()).toBeUndefined();
  });

  it("writes a provider id (never a credential) as the default provider", async () => {
    const user = setupUser();
    stubSettings({ providers: [PROVIDER_A, PROVIDER_B] });
    await renderPanel();

    await user.selectOptions(
      screen.getByLabelText("Default provider"),
      "prov-2",
    );

    expect(lastWrite()).toEqual({
      key: "workspaces.defaultProviderId",
      value: "prov-2",
    });
  });

  it("resets a row by writing the empty value that deletes it", async () => {
    const user = setupUser();
    stubSettings({ preferences: { "terminal.fontSize": "18" } });
    await renderPanel();

    await user.click(resetButton("terminal.fontSize"));

    // An empty value is the backend's delete (spec section 14).
    expect(lastWrite()).toEqual({ key: "terminal.fontSize", value: "" });
    // The field follows the value back to the default in the same breath.
    expect(screen.getByLabelText("Font size in pixels")).toHaveValue(12);
    expect(resetButton("terminal.fontSize")).toBeDisabled();
  });

  it("rolls the control back and reports the reason when the write fails", async () => {
    const user = setupUser();
    stubCommands({
      list_ui_preferences: () => ({ "terminal.fontSize": "18" }),
      set_ui_preference: () => {
        throw "database is locked";
      },
      app_info: () => APP_INFO,
      list_providers: () => [],
      provider_secret_status: () => false,
    });
    await renderPanel();
    const field = screen.getByLabelText("Font size in pixels");

    await user.clear(field);
    await user.type(field, "20");
    await user.keyboard("{Enter}");

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "database is locked",
    );
    // Rolled back: a failed write must not leave the panel claiming a value that
    // is not stored.
    await waitFor(() => expect(field).toHaveValue(18));
  });
});

describe("SettingsPanel: About / Storage", () => {
  it("reports the real version, paths, database size and schema version", async () => {
    stubSettings();
    render(<SettingsPanel />);

    expect(await screen.findByText("0.3.1")).toBeInTheDocument();
    expect(screen.getByText(APP_INFO.dataDirectory)).toBeInTheDocument();
    expect(screen.getByText(APP_INFO.databasePath)).toBeInTheDocument();
    expect(screen.getByText(/2\.5 MiB/)).toBeInTheDocument();
    expect(screen.getByText("4")).toBeInTheDocument();
    // One row per agent this build implements, from the backend registry.
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(
      screen.getByText(APP_INFO.agents[0].executablePath as string),
    ).toBeInTheDocument();
  });

  it("says which agent CLIs are missing rather than showing empty paths", async () => {
    stubSettings({
      appInfo: {
        ...APP_INFO,
        agents: [
          makeAgent({ executablePath: null, installed: false }),
          makeAgent({
            id: "codex",
            name: "Codex",
            executablePath: null,
            installed: false,
          }),
        ],
      },
    });
    render(<SettingsPanel />);

    await screen.findByText("Claude Code");
    expect(screen.getAllByText("not found on PATH")).toHaveLength(2);
  });

  it("omits the size when the database file does not exist yet", async () => {
    stubSettings({ appInfo: { ...APP_INFO, databaseSizeBytes: null } });
    render(<SettingsPanel />);

    expect(await screen.findByText(APP_INFO.databasePath)).toBeInTheDocument();
    expect(screen.queryByText(/KiB|MiB/)).not.toBeInTheDocument();
  });

  it("opens the data folder on request, and reports a refusal inline", async () => {
    const user = setupUser();
    stubSettings();
    render(<SettingsPanel />);

    await user.click(await screen.findByRole("button", { name: "Open data folder" }));
    expect(invokeMock).toHaveBeenCalledWith("open_data_directory", undefined);
    // A failure is visible: "nothing happened" is otherwise indistinguishable
    // from a slow file manager.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    stubCommands({
      list_ui_preferences: () => ({}),
      app_info: () => APP_INFO,
      list_providers: () => [],
      provider_secret_status: () => false,
      open_data_directory: () => {
        throw "no file manager available";
      },
    });
    await user.click(screen.getByRole("button", { name: "Open data folder" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "no file manager available",
    );
  });
});

describe("SettingsPanel: without the Rust core", () => {
  it("explains that nothing can be stored and disables every control", async () => {
    tauriRuntime.available = false;
    render(<SettingsPanel />);

    expect(
      await screen.findByLabelText("Backend unavailable"),
    ).toBeInTheDocument();
    // The header says which state the panel is in, next to its title.
    const header = screen
      .getByRole("heading", { name: "Settings" })
      .closest(".panel-header") as HTMLElement;
    expect(within(header).getByText("backend unavailable")).toBeInTheDocument();

    // Every control is inert, and no write is attempted.
    expect(screen.getByLabelText("Font size in pixels")).toBeDisabled();
    expect(screen.getByLabelText("Blinking cursor")).toBeDisabled();
    expect(screen.getByLabelText("Restore tabs at launch")).toBeDisabled();
    expect(screen.getByLabelText("Default provider")).toBeDisabled();
    for (const choice of screen.getAllByRole("radio")) {
      expect(choice).toBeDisabled();
    }
    expect(resetButton("terminal.fontSize")).toBeDisabled();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("still renders the defaults, so the layout stays inspectable", async () => {
    tauriRuntime.available = false;
    render(<SettingsPanel />);

    expect(screen.getByLabelText("Font size in pixels")).toHaveValue(
      preferences.terminalFontSize.defaultValue,
    );
    expect(
      await screen.findByText("Unavailable without the backend."),
    ).toBeInTheDocument();
  });
});
