/**
 * Tauri transport layer.
 *
 * Every call into the Rust core goes through `call()`, which exists so the UI
 * degrades gracefully: in a plain browser (`npm run dev`, `npm run preview`)
 * there is no Rust core, `invoke` would reject, and the panels would otherwise
 * crash or show a confusing error. Instead the missing backend is detected up
 * front and reported as a `BackendUnavailableError` that panels render as a
 * "run the desktop app" notice.
 *
 * Error text from commands is already user-facing and secret-free: the Rust
 * commands return `Result<_, String>` built from error types that never carry
 * credentials (spec sections 5, 16, 17).
 */

import { invoke, isTauri } from "@tauri-apps/api/core";

/** Message shown wherever the desktop backend is missing. */
export const BACKEND_UNAVAILABLE_MESSAGE =
  "Backend unavailable - the React UI is running without the Rust core. " +
  "Start the desktop app with `npm run tauri dev` to manage providers.";

/** Thrown when a command is attempted outside the Tauri runtime. */
export class BackendUnavailableError extends Error {
  constructor(message: string = BACKEND_UNAVAILABLE_MESSAGE) {
    super(message);
    this.name = "BackendUnavailableError";
  }
}

/**
 * Whether a Tauri backend is reachable.
 *
 * `isTauri()` covers Tauri 2's own flag; the `__TAURI_INTERNALS__` check is a
 * belt-and-braces fallback so a version difference cannot make the UI think a
 * backend exists when it does not.
 */
export function isBackendAvailable(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  const scope = window as unknown as { __TAURI_INTERNALS__?: unknown };
  return isTauri() || scope.__TAURI_INTERNALS__ !== undefined;
}

/** Whether `error` was raised because the Tauri backend is missing. */
export function isBackendUnavailableError(
  error: unknown,
): error is BackendUnavailableError {
  return error instanceof BackendUnavailableError;
}

/**
 * Render any thrown value as a displayable message.
 *
 * Tauri rejects a `Result<_, String>` command with the plain string; a JS error
 * keeps its message.
 */
export function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  return "unexpected error";
}

/**
 * Invoke a Rust command, normalizing failures.
 *
 * @throws {BackendUnavailableError} when not running inside Tauri.
 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isBackendAvailable()) {
    throw new BackendUnavailableError();
  }
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}
