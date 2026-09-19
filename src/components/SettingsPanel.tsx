/**
 * The Settings tab (spec section 12).
 *
 * Five sections, all backed by the `ui_preferences` table through
 * `list_ui_preferences` / `set_ui_preference`:
 *
 * - **Appearance** - the palette, applied to the whole UI and the terminal.
 * - **Terminal** - font size, scrollback, cursor blink, copy-on-select.
 * - **Sessions** - restore the tab set at launch, confirm before closing a
 *   running session.
 * - **Workspaces** - the provider preselected in the New Workspace dialog.
 * - **About / Storage** - version, paths, database size, schema version.
 *
 * ## How a setting is saved
 *
 * Every control writes on change; there is no Save button. That is what the
 * backend's `set_ui_preference` is shaped for - an empty value *deletes* the
 * row, so "unset" and "set to empty" are the same thing and the frontend default
 * applies again. Each row therefore carries a Reset action that writes the empty
 * value, and Reset is disabled while the row is already at its default.
 *
 * No control here can touch a credential (spec sections 5, 17): every value in
 * this panel is a palette name, a number, a boolean, or a provider **id**.
 */

import { useEffect, useState, type ReactNode } from "react";
import { useAppStore } from "../stores/useAppStore";
import BackendNotice from "./BackendNotice";
import {
  formatBytes,
  preferences,
  type Preference,
} from "../settings/preferences";
import { settingsApi } from "../services/settings";
import { errorMessage } from "../services/backend";
import type { ThemeMode } from "../types";

/** The three palette choices, in the order they are offered. */
const THEME_CHOICES: { mode: ThemeMode; label: string; hint: string }[] = [
  {
    mode: "dark",
    label: "Dark",
    hint: "Always the dark palette.",
  },
  {
    mode: "light",
    label: "Light",
    hint: "Always the light palette.",
  },
  {
    mode: "system",
    label: "System",
    hint: "Follow the operating system and change with it.",
  },
];

/**
 * Read a preference as its typed value, with the actions that write it back.
 *
 * The typed value comes from `preference.get`, which owns the default and the
 * parsing - this hook deliberately never touches the raw string.
 */
function usePreference<T>(preference: Preference<T>) {
  const value = useAppStore((state) => preference.get(state.preferences));
  // "Is this row at its default?" is a question about the *stored* map, not the
  // parsed value: a key holding something unparseable reads as the default but
  // is still stored, and Reset should clear it.
  const isDefault = useAppStore(
    (state) => state.preferences[preference.key] === undefined,
  );
  const setPreference = useAppStore((state) => state.setPreference);
  const resetPreference = useAppStore((state) => state.resetPreference);

  return {
    value,
    isDefault,
    /**
     * Write a value, resolving `false` when the backend refused it.
     *
     * Most controls can ignore the answer - the store has already rolled itself
     * back, so the control simply re-renders on the old value. A control that
     * holds a *draft* while the user types cannot: it has to be told, or it
     * would keep showing a number that is not stored anywhere.
     */
    set: (next: T) =>
      setPreference(preference.key, preference.serialize(next)),
    reset: () => resetPreference(preference.key),
  };
}

export default function SettingsPanel() {
  const backendStatus = useAppStore((state) => state.backendStatus);
  const preferencesLoaded = useAppStore((state) => state.preferencesLoaded);
  const preferencesError = useAppStore((state) => state.preferencesError);
  const providers = useAppStore((state) => state.providers);
  const ensurePreferencesLoaded = useAppStore(
    (state) => state.ensurePreferencesLoaded,
  );
  const ensureProvidersLoaded = useAppStore((state) => state.ensureProvidersLoaded);
  const loadAppInfo = useAppStore((state) => state.loadAppInfo);
  const appInfo = useAppStore((state) => state.appInfo);

  const theme = usePreference(preferences.appearanceTheme);
  const fontSize = usePreference(preferences.terminalFontSize);
  const scrollback = usePreference(preferences.terminalScrollback);
  const cursorBlink = usePreference(preferences.terminalCursorBlink);
  const copyOnSelect = usePreference(preferences.terminalCopyOnSelect);
  const restoreTabs = usePreference(preferences.sessionsRestoreTabs);
  const confirmCloseRunning = usePreference(
    preferences.sessionsConfirmCloseRunning,
  );
  const defaultProvider = usePreference(preferences.workspacesDefaultProvider);

  useEffect(() => {
    ensurePreferencesLoaded();
    // The Workspaces section selects from the configured profiles, so the list
    // has to be loaded even if the user never opened the Providers tab.
    ensureProvidersLoaded();
    void loadAppInfo();
  }, [ensurePreferencesLoaded, ensureProvidersLoaded, loadAppInfo]);

  const backendMissing = backendStatus === "unavailable";
  // Outside Tauri nothing persists, and the About section has no paths to
  // report; the controls stay visible but inert so the layout is inspectable.
  const disabled = backendMissing;

  return (
    <section className="settings-panel" aria-label="Settings">
      <header className="panel-header">
        <h2>Settings</h2>
        <span className="muted">
          {backendMissing
            ? "backend unavailable"
            : preferencesLoaded
              ? "stored in the application database"
              : "loading…"}
        </span>
      </header>

      {backendMissing ? <BackendNotice message={preferencesError ?? undefined} /> : null}

      {!backendMissing && preferencesError !== null ? (
        <p className="alert alert-error" role="alert">
          {preferencesError}
        </p>
      ) : null}

      <section className="settings-section" aria-labelledby="settings-appearance">
        <h3 id="settings-appearance">Appearance</h3>
        <SettingRow
          label="Theme"
          hint="Applies to the whole window, including the terminal."
          preferenceKey={preferences.appearanceTheme.key}
          onReset={theme.reset}
          resetDisabled={theme.isDefault}
        >
          <div
            className="settings-row-control"
            role="radiogroup"
            aria-label="Theme"
          >
            {THEME_CHOICES.map((choice) => (
              <label
                key={choice.mode}
                className="theme-choice"
                title={choice.hint}
              >
                <input
                  type="radio"
                  name="theme"
                  value={choice.mode}
                  checked={theme.value === choice.mode}
                  disabled={disabled}
                  onChange={() => theme.set(choice.mode)}
                />
                {choice.label}
              </label>
            ))}
          </div>
        </SettingRow>
      </section>

      <section className="settings-section" aria-labelledby="settings-terminal">
        <h3 id="settings-terminal">Terminal</h3>

        <SettingRow
          label="Font size"
          hint={`${preferences.terminalFontSize.min}–${preferences.terminalFontSize.max} px. Applies to open terminals immediately.`}
          preferenceKey={preferences.terminalFontSize.key}
          onReset={fontSize.reset}
          resetDisabled={fontSize.isDefault}
        >
          <NumberField
            id="setting-terminal-font-size"
            label="Font size in pixels"
            value={fontSize.value}
            min={preferences.terminalFontSize.min}
            max={preferences.terminalFontSize.max}
            clamp={preferences.terminalFontSize.clamp}
            disabled={disabled}
            onCommit={fontSize.set}
          />
        </SettingRow>

        <SettingRow
          label="Scrollback"
          hint={`${preferences.terminalScrollback.min.toLocaleString("en-US")}–${preferences.terminalScrollback.max.toLocaleString("en-US")} lines. Applies to terminals opened after the change.`}
          preferenceKey={preferences.terminalScrollback.key}
          onReset={scrollback.reset}
          resetDisabled={scrollback.isDefault}
        >
          <NumberField
            id="setting-terminal-scrollback"
            label="Scrollback lines"
            value={scrollback.value}
            min={preferences.terminalScrollback.min}
            max={preferences.terminalScrollback.max}
            clamp={preferences.terminalScrollback.clamp}
            disabled={disabled}
            onCommit={scrollback.set}
          />
        </SettingRow>

        <SettingRow
          label="Blinking cursor"
          hint="Turn off for a steady block cursor."
          preferenceKey={preferences.terminalCursorBlink.key}
          onReset={cursorBlink.reset}
          resetDisabled={cursorBlink.isDefault}
        >
          <input
            id="setting-terminal-cursor-blink"
            type="checkbox"
            aria-label="Blinking cursor"
            checked={cursorBlink.value}
            disabled={disabled}
            onChange={(event) => cursorBlink.set(event.target.checked)}
          />
        </SettingRow>

        <SettingRow
          label="Copy on select"
          hint="Copy the selection as soon as it is made. Ctrl+C, Ctrl+Shift+C and the right-click menu work either way."
          preferenceKey={preferences.terminalCopyOnSelect.key}
          onReset={copyOnSelect.reset}
          resetDisabled={copyOnSelect.isDefault}
        >
          <input
            id="setting-terminal-copy-on-select"
            type="checkbox"
            aria-label="Copy on select"
            checked={copyOnSelect.value}
            disabled={disabled}
            onChange={(event) => copyOnSelect.set(event.target.checked)}
          />
        </SettingRow>
      </section>

      <section className="settings-section" aria-labelledby="settings-sessions">
        <h3 id="settings-sessions">Sessions</h3>

        <SettingRow
          label="Restore tabs at launch"
          hint="Reopen the tabs that were open when the app last closed. Sessions always come back stopped."
          preferenceKey={preferences.sessionsRestoreTabs.key}
          onReset={restoreTabs.reset}
          resetDisabled={restoreTabs.isDefault}
        >
          <input
            id="setting-sessions-restore-tabs"
            type="checkbox"
            aria-label="Restore tabs at launch"
            checked={restoreTabs.value}
            disabled={disabled}
            onChange={(event) => restoreTabs.set(event.target.checked)}
          />
        </SettingRow>

        <SettingRow
          label="Confirm before closing a running session"
          hint="Asks before a tab whose agent is still running is closed, which stops that session."
          preferenceKey={preferences.sessionsConfirmCloseRunning.key}
          onReset={confirmCloseRunning.reset}
          resetDisabled={confirmCloseRunning.isDefault}
        >
          <input
            id="setting-sessions-confirm-close-running"
            type="checkbox"
            aria-label="Confirm before closing a running session"
            checked={confirmCloseRunning.value}
            disabled={disabled}
            onChange={(event) => confirmCloseRunning.set(event.target.checked)}
          />
        </SettingRow>
      </section>

      <section className="settings-section" aria-labelledby="settings-workspaces">
        <h3 id="settings-workspaces">Workspaces</h3>

        <SettingRow
          label="Default provider"
          hint="Preselected in the New Workspace dialog. Each workspace can still choose another."
          preferenceKey={preferences.workspacesDefaultProvider.key}
          onReset={defaultProvider.reset}
          resetDisabled={defaultProvider.isDefault}
        >
          <select
            id="setting-workspaces-default-provider"
            aria-label="Default provider"
            value={defaultProvider.value}
            disabled={disabled || providers.length === 0}
            onChange={(event) => defaultProvider.set(event.target.value)}
          >
            {/* The empty value is a real choice, not a placeholder: it means
                "no preference", which is what the dialog did before this
                setting existed. */}
            <option value="">First configured provider</option>
            {providers.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.name}
              </option>
            ))}
          </select>
        </SettingRow>
        {providers.length === 0 ? (
          <p className="field-hint muted">
            No provider profiles configured yet.
          </p>
        ) : null}
      </section>

      <section className="settings-section" aria-labelledby="settings-about">
        <h3 id="settings-about">About / Storage</h3>

        {appInfo === null ? (
          <p className="muted">
            {backendMissing ? "Unavailable without the backend." : "Loading…"}
          </p>
        ) : (
          <>
            <dl className="settings-about">
              <dt>Version</dt>
              <dd className="mono">{appInfo.version}</dd>

              <dt>Data directory</dt>
              <dd className="mono">{appInfo.dataDirectory}</dd>

              <dt>Database</dt>
              <dd className="mono">
                {appInfo.databasePath}
                {appInfo.databaseSizeBytes !== null ? (
                  <span className="muted">
                    {" "}
                    ({formatBytes(appInfo.databaseSizeBytes)})
                  </span>
                ) : null}
              </dd>

              <dt>Schema version</dt>
              <dd className="mono">{appInfo.schemaVersion}</dd>

              <dt>Claude Code</dt>
              <dd className="mono">
                {appInfo.claudeCodePath ?? (
                  <span className="muted">not found on PATH</span>
                )}
              </dd>
            </dl>

            <div className="settings-about-actions">
              <OpenDataDirectoryButton disabled={disabled} />
            </div>
          </>
        )}
      </section>
    </section>
  );
}

interface SettingRowProps {
  label: string;
  hint: string;
  /** The backend key, used to build a stable test id. */
  preferenceKey: string;
  onReset: () => void;
  resetDisabled: boolean;
  children: ReactNode;
}

/**
 * One setting: what it is on the left, its control and reset action on the right.
 *
 * Reset is disabled rather than hidden while the row is at its default, so the
 * controls do not shift sideways as values are changed.
 */
function SettingRow({
  label,
  hint,
  preferenceKey,
  onReset,
  resetDisabled,
  children,
}: SettingRowProps) {
  return (
    <div className="settings-row" data-setting={preferenceKey}>
      <div className="settings-row-label">
        <span>{label}</span>
        <span className="field-hint muted">{hint}</span>
      </div>
      <div className="settings-row-control">
        {children}
        <button
          type="button"
          className="btn btn-quiet"
          onClick={onReset}
          disabled={resetDisabled}
          title={
            resetDisabled ? "Already at its default" : "Reset to the default"
          }
        >
          Reset
        </button>
      </div>
    </div>
  );
}

interface NumberFieldProps {
  id: string;
  /** Accessible name, also what tests match on. */
  label: string;
  value: number;
  min: number;
  max: number;
  clamp: (value: number) => number;
  disabled: boolean;
  /** Resolves `false` when the backend refused the write. */
  onCommit: (value: number) => Promise<boolean>;
}

/**
 * A whole-number setting.
 *
 * Keeps a local draft while the user types, and commits on blur or Enter. A
 * directly-controlled input would clamp on every keystroke, so typing "18" into
 * a field with a minimum of 8 would rewrite the "1" to "8" before the "8" could
 * be typed. The draft also means a partially typed value is never written to the
 * database.
 */
function NumberField({
  id,
  label,
  value,
  min,
  max,
  clamp,
  disabled,
  onCommit,
}: NumberFieldProps) {
  const [draft, setDraft] = useState(String(value));

  // Follow the stored value when it changes from elsewhere (a Reset, or a
  // failed write rolling back) - but not while the field is being typed into,
  // which is why this only runs when the committed value itself changes.
  useEffect(() => {
    setDraft(String(value));
  }, [value]);

  function commit() {
    const parsed = Number(draft);
    if (draft.trim() === "" || !Number.isFinite(parsed)) {
      // Nothing usable was typed: put the stored value back rather than writing
      // a guess.
      setDraft(String(value));
      return;
    }
    const clamped = clamp(parsed);
    setDraft(String(clamped));
    if (clamped === value) {
      return;
    }
    // A refused write rolls the store back to `value` - which the effect above
    // cannot see, because from React's point of view the value never changed. The
    // draft has to be put back here, or the field would sit on a number that is
    // stored nowhere.
    void onCommit(clamped).then((accepted) => {
      if (!accepted) {
        setDraft(String(value));
      }
    });
  }

  return (
    <input
      id={id}
      type="number"
      inputMode="numeric"
      aria-label={label}
      value={draft}
      min={min}
      max={max}
      step={1}
      disabled={disabled}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          // Enter must not submit anything - the panel has no form - so the
          // value is committed and the event consumed.
          event.preventDefault();
          commit();
          event.currentTarget.blur();
        }
        if (event.key === "Escape") {
          setDraft(String(value));
        }
      }}
    />
  );
}

/**
 * Open the application data directory in the OS file manager.
 *
 * A pure side effect with no state of its own, so it calls the service directly
 * rather than going through the store - the same shape as the folder picker in
 * `WorkspaceDialog`. A failure is reported inline, because "nothing happened"
 * when clicking this button is indistinguishable from a slow file manager.
 */
function OpenDataDirectoryButton({ disabled }: { disabled: boolean }) {
  const [error, setError] = useState<string | null>(null);

  return (
    <>
      <button
        type="button"
        className="btn"
        disabled={disabled}
        onClick={() => {
          setError(null);
          void settingsApi.openDataDirectory().catch((cause: unknown) => {
            setError(errorMessage(cause));
          });
        }}
      >
        Open data folder
      </button>
      {error !== null ? (
        <p className="alert alert-error" role="alert">
          {error}
        </p>
      ) : null}
    </>
  );
}
