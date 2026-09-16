/**
 * Typed wrappers over the session commands (spec sections 8, 11, 15).
 *
 * These mirror the command names the Rust session manager is expected to
 * expose (`start_session`, `stop_session`, …) and follow the same conventions
 * as `services/providers.ts`: snake_case command names, camelCase argument keys
 * (Tauri 2 converts Rust parameters to camelCase), every call through `call()`
 * so a missing backend surfaces as `BackendUnavailableError` instead of a raw
 * `invoke` rejection.
 *
 * Nothing here is allowed to throw for "no Tauri runtime": the components show
 * a notice and keep the layout inspectable, so availability is answered by
 * `isBackendAvailable()` and every subscription degrades to a no-op unlisten.
 *
 * NOTE: the Rust side of M3 is not written yet, so the argument names and the
 * payload shapes below are a contract proposal. The output/state subscribes are
 * deliberately tolerant (see `normalizeOutput` / `normalizeState`) so a rename
 * on the Rust side degrades to missing live output rather than a crash.
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { call, isBackendAvailable } from "./backend";
import type {
  SessionInfo,
  SessionStateEvent,
  TerminalSessionStatus,
} from "../types";

/** Event channel carrying raw PTY output for one session. */
export function sessionOutputChannel(sessionId: string): string {
  return `session-output:${sessionId}`;
}

/** Event channel carrying lifecycle transitions for one session. */
export function sessionStateChannel(sessionId: string): string {
  return `session-state:${sessionId}`;
}

const SESSION_STATUSES: readonly TerminalSessionStatus[] = [
  "created",
  "starting",
  "running",
  "stopping",
  "stopped",
  "failed",
];

/**
 * Narrow a value coming from the backend to a known status.
 *
 * The Rust session manager does not exist yet, so a typo'd or added status must
 * degrade to "unknown, keep the previous value" rather than render a blank chip.
 */
export function isSessionStatus(
  value: unknown,
): value is TerminalSessionStatus {
  return (
    typeof value === "string" &&
    (SESSION_STATUSES as readonly string[]).includes(value)
  );
}

/** Command wrappers, one per Rust session command. */
export const sessionsApi = {
  /** Spawn the agent for a workspace. Returns the new session's id/state. */
  start: (workspaceId: string): Promise<SessionInfo> =>
    call<SessionInfo>("start_session", { workspaceId }),

  /** Stop a session (SIGTERM-equivalent, then the PTY is dropped). */
  stop: (sessionId: string): Promise<void> =>
    call<void>("stop_session", { sessionId }),

  /** Stop and re-spawn, keeping the same tab. */
  restart: (sessionId: string): Promise<SessionInfo> =>
    call<SessionInfo>("restart_session", { sessionId }),

  /** Forward keystrokes to the PTY. */
  write: (sessionId: string, data: string): Promise<void> =>
    call<void>("write_session", { sessionId, data }),

  /** Tell the PTY its new size so full-screen TUIs reflow correctly. */
  resize: (sessionId: string, cols: number, rows: number): Promise<void> =>
    call<void>("resize_session", { sessionId, cols, rows }),

  /** Every live session; used to re-attach after a UI reload. */
  list: (): Promise<SessionInfo[]> => call<SessionInfo[]>("list_sessions"),
};

/**
 * Raw `session-output` payload.
 *
 * The Rust event emitter may send a bare string of PTY bytes or a small struct;
 * `normalizeOutput` accepts both.
 */
type RawOutputPayload = string | { data?: string; chunk?: string };

function normalizeOutput(payload: RawOutputPayload): string | null {
  if (typeof payload === "string") {
    return payload;
  }
  if (payload && typeof payload === "object") {
    if (typeof payload.data === "string") {
      return payload.data;
    }
    if (typeof payload.chunk === "string") {
      return payload.chunk;
    }
  }
  return null;
}

function normalizeState(
  sessionId: string,
  payload: Partial<SessionStateEvent> | string,
): SessionStateEvent | null {
  // A bare string is read as the new status.
  if (isSessionStatus(payload)) {
    return { sessionId, status: payload, exitCode: null };
  }
  if (payload && typeof payload === "object" && isSessionStatus(payload.status)) {
    return {
      sessionId: payload.sessionId ?? sessionId,
      status: payload.status,
      exitCode: payload.exitCode ?? null,
    };
  }
  return null;
}

/**
 * Subscribe to whatever helper is appropriate for the situation.
 *
 * Outside Tauri (or if the event plugin rejects) this resolves to a no-op
 * unlisten: a session that is otherwise working must not fail because live
 * updates could not be wired up.
 */
async function subscribe<T>(
  channel: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  if (!isBackendAvailable()) {
    return () => {};
  }
  try {
    return await listen<T>(channel, (event) => handler(event.payload));
  } catch {
    return () => {};
  }
}

/**
 * Stream a session's PTY output.
 *
 * @returns an unlisten function; safe to call even if the subscription failed.
 */
export async function onSessionOutput(
  sessionId: string,
  handler: (data: string) => void,
): Promise<UnlistenFn> {
  return subscribe<RawOutputPayload>(sessionOutputChannel(sessionId), (raw) => {
    const data = normalizeOutput(raw);
    if (data !== null && data.length > 0) {
      handler(data);
    }
  });
}

/**
 * Observe a session's lifecycle transitions (status chip, tab dot).
 *
 * @returns an unlisten function; safe to call even if the subscription failed.
 */
export async function onSessionState(
  sessionId: string,
  handler: (state: SessionStateEvent) => void,
): Promise<UnlistenFn> {
  return subscribe<Partial<SessionStateEvent> | string>(
    sessionStateChannel(sessionId),
    (raw) => {
      const state = normalizeState(sessionId, raw);
      if (state !== null) {
        handler(state);
      }
    },
  );
}
