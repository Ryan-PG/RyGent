/**
 * Fake Tauri bridge for the frontend suite.
 *
 * There is no Rust core in jsdom and `npm test` must never talk to a real
 * backend (or spawn `claude`), so `@tauri-apps/api/core` and
 * `@tauri-apps/api/event` are replaced by the fakes below.
 *
 * Only the *bridge* is faked. `services/*`, the store and the components run
 * their real code - including `call()`'s error normalization - so stubbing
 * `list_workspaces` exercises the same path the app uses, and a command a test
 * forgot to stub rejects loudly instead of silently returning `undefined`.
 *
 * Usage in a test file (the factories are read through a dynamic `import` so
 * `vi.mock`'s hoisting cannot see them before this module is initialized):
 *
 *   vi.mock("@tauri-apps/api/core", async () =>
 *     (await import("./mock-tauri")).tauriCoreModule());
 */

import { vi } from "vitest";

/** Whether the faked Rust core is present, as the real `isTauri()` reports it. */
export const tauriRuntime = { available: true };

/** Arguments a Rust command receives (Tauri 2 converts them to camelCase). */
export type CommandArgs = Record<string, unknown> | undefined;

/** Handler for one Rust command: return a value to resolve, throw to reject. */
export type CommandHandler = (args: CommandArgs) => unknown;

/** Command name -> handler; anything missing rejects. */
export type CommandTable = Record<string, CommandHandler>;

const UNSTUBBED = (command: string) => `unstubbed Tauri command "${command}"`;

/** `invoke` replacement: routes to the table installed by `stubCommands`. */
export const invokeMock = vi.fn<
  (command: string, args?: CommandArgs) => Promise<unknown>
>(async (command) => {
  throw new Error(UNSTUBBED(command));
});

interface Subscription {
  channel: string;
  handler: (event: { payload: unknown }) => void;
  active: boolean;
}

const subscriptions: Subscription[] = [];

/** `listen` replacement: records subscriptions so tests can emit events. */
export const listenMock = vi.fn<
  (
    channel: string,
    handler: (event: { payload: unknown }) => void,
  ) => Promise<() => void>
>(async (channel, handler) => {
  const subscription: Subscription = { channel, handler, active: true };
  subscriptions.push(subscription);
  return () => {
    subscription.active = false;
  };
});

/**
 * Install a command table.
 *
 * An unstubbed command throws, which the store turns into a banner error - a
 * test that expects success therefore fails instead of quietly passing against
 * a `undefined` payload.
 */
export function stubCommands(table: CommandTable): void {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command, args) => {
    const handler = table[command];
    if (handler === undefined) {
      throw new Error(UNSTUBBED(command));
    }
    return await handler(args);
  });
}

/** Deliver an event to every live subscription of one channel. */
export function emitTauriEvent(channel: string, payload: unknown): void {
  for (const subscription of [...subscriptions]) {
    if (subscription.active && subscription.channel === channel) {
      subscription.handler({ payload });
    }
  }
}

/** Channels that currently have a listener (for asserting subscriptions). */
export function subscribedChannels(): string[] {
  return subscriptions
    .filter((subscription) => subscription.active)
    .map((subscription) => subscription.channel);
}

/** `open()` replacement for the fake `@tauri-apps/plugin-dialog`. */
export const dialogOpenMock = vi.fn<() => Promise<unknown>>(async () => null);

/** Back to the default bridge: backend present, nothing stubbed, no listeners. */
export function resetTauriMock(): void {
  tauriRuntime.available = true;
  subscriptions.length = 0;
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command) => {
    throw new Error(UNSTUBBED(command));
  });
  listenMock.mockReset();
  listenMock.mockImplementation(async (channel, handler) => {
    const subscription: Subscription = { channel, handler, active: true };
    subscriptions.push(subscription);
    return () => {
      subscription.active = false;
    };
  });
  // Default for the native folder picker: dismissed without a selection.
  dialogOpenMock.mockReset();
  dialogOpenMock.mockImplementation(async () => null);
}

/** Fake `@tauri-apps/api/core`. */
export function tauriCoreModule(): Record<string, unknown> {
  return {
    invoke: invokeMock,
    isTauri: () => tauriRuntime.available,
  };
}

/** Fake `@tauri-apps/api/event`. */
export function tauriEventModule(): Record<string, unknown> {
  return {
    listen: listenMock,
    once: listenMock,
    emit: vi.fn(async () => undefined),
  };
}

/** Fake `@tauri-apps/plugin-dialog` (the native folder picker). */
export function tauriDialogModule(): Record<string, unknown> {
  return { open: dialogOpenMock };
}
