/**
 * Shared fixtures and store reset for the frontend suite.
 *
 * The zustand store is a module singleton, so tests share it. `resetAppStore`
 * puts it back to the state the app boots with between tests; state updates in
 * the store are immutable, so the snapshot taken at import time stays pristine.
 *
 * Seed helpers prefer the store's *real* actions (`openWorkspace`) over
 * hand-built objects wherever that is possible, so a test never asserts against
 * a fixture that has drifted from what the app would actually produce.
 */

import userEvent from "@testing-library/user-event";
import { AGENT_ID, useAppStore } from "../stores/useAppStore";
import type { Preference } from "../settings/preferences";
import type {
  ProviderProfile,
  SecretStatus,
  TerminalSession,
  UiPreferences,
  Workspace,
  WorkspaceTab,
} from "../types";

/**
 * A user-event driver with no inter-event delay.
 *
 * The default delay waits a real timer tick between keystrokes, which both
 * slows the suite down and introduces wall-clock waiting; `delay: null` removes
 * it while still dispatching the full pointer/keyboard event sequence.
 */
export function setupUser(): ReturnType<typeof userEvent.setup> {
  return userEvent.setup({ delay: null });
}

/** The state the store is created with, captured before any test runs. */
const INITIAL_STATE = useAppStore.getState();

/** Put the store back to its boot state. */
export function resetAppStore(): void {
  useAppStore.setState(INITIAL_STATE, true);
}

/** A persisted workspace with valid defaults; override what a test cares about. */
export function makeWorkspace(overrides: Partial<Workspace> = {}): Workspace {
  return {
    id: "ws-1",
    name: "Alpha",
    projectPath: "D:\\Projects\\alpha",
    agentId: AGENT_ID,
    providerId: "prov-1",
    model: null,
    ...overrides,
  };
}

/** A provider profile (metadata only - a profile never carries a key). */
export function makeProvider(
  overrides: Partial<ProviderProfile> = {},
): ProviderProfile {
  return {
    id: "prov-1",
    name: "Provider A",
    baseUrl: "https://provider-a.example.com",
    model: "model-a",
    extraEnv: [],
    // The backend serializes the optional column as `null`, never as an absent
    // key, so the default fixture matches what `list_providers` really returns.
    maxContextTokens: null,
    ...overrides,
  };
}

/** Seed the workspace list the way a successful `list_workspaces` would. */
export function seedWorkspaces(
  workspaces: Workspace[],
  openIds: string[] = [],
): void {
  useAppStore.setState({ workspaces, workspacesLoaded: true });
  for (const id of openIds) {
    // The real action builds the tab, so tab metadata always matches the app.
    useAppStore.getState().openWorkspace(id);
  }
}

/** Seed the provider list + keyring status the way `loadProviders` would. */
export function seedProviders(
  providers: ProviderProfile[],
  secretStatus: Record<string, SecretStatus> = {},
): void {
  useAppStore.setState({
    providers,
    secretStatus,
    providersLoaded: true,
    backendStatus: "ready",
  });
}

/** Seed one tab's session state. */
export function seedSession(
  tabId: string,
  session: Partial<TerminalSession> = {},
): void {
  const sessions = useAppStore.getState().sessions;
  useAppStore.setState({
    sessions: {
      ...sessions,
      [tabId]: { status: "created", cols: 80, rows: 24, ...session },
    },
  });
}

/** The tab the store opened for a workspace (throws if it has none). */
export function tabFor(workspaceId: string): WorkspaceTab {
  const tab = useAppStore
    .getState()
    .tabs.find((entry) => entry.id === workspaceId);
  if (tab === undefined) {
    throw new Error(`no open tab for workspace "${workspaceId}"`);
  }
  return tab;
}

/**
 * Seed the preference map the way a successful `list_ui_preferences` would.
 *
 * The values are the **stored strings**, not typed values, which is what makes
 * this useful for the fallback tests: seeding `{ "terminal.fontSize": "huge" }`
 * is the only way to prove a malformed value reads as the default rather than
 * breaking the panel. For the ordinary case, prefer `seedPreference`, which
 * serializes through the same module the app writes with.
 */
export function seedPreferences(stored: UiPreferences = {}): void {
  useAppStore.setState({
    preferences: { ...stored },
    preferencesLoaded: true,
    preferencesLoading: false,
  });
}

/** Seed one preference from a typed value, serialized as the app would. */
export function seedPreference<T>(preference: Preference<T>, value: T): void {
  const current = useAppStore.getState().preferences;
  useAppStore.setState({
    preferences: {
      ...current,
      [preference.key]: preference.serialize(value),
    },
    preferencesLoaded: true,
    preferencesLoading: false,
  });
}
