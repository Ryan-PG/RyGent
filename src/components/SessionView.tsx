/**
 * The agent session view (spec sections 10, 11, 12).
 *
 * Owns the toolbar (start / stop / restart, lifecycle chip) and the embedded
 * terminal, and is the only place that talks to the session service at
 * runtime:
 *
 * - keystrokes  -> `store.writeSessionInput` -> `write_session`
 * - terminal size -> `store.resizeSession`   -> `resize_session`
 * - PTY output  -> written straight into xterm, bypassing React state
 * - lifecycle   -> `session-state:<id>` events -> `store.applySessionState`
 *
 * When there is no Tauri runtime the view stays mounted and usable: the
 * terminal still renders, the toolbar still responds, and every action prints
 * an explanatory line instead of failing. That keeps the layout inspectable in
 * a plain browser, which is how the frontend half of M3 was verified.
 */

import { useCallback, useEffect, useRef } from "react";
import BackendNotice from "./BackendNotice";
import SessionTerminal, {
  type SessionTerminalHandle,
} from "./SessionTerminal";
import { useAppStore } from "../stores/useAppStore";
import { isBackendAvailable } from "../services/backend";
import { onSessionOutput, onSessionState } from "../services/sessions";
import type { TerminalSessionStatus, WorkspaceTab } from "../types";
import type { UnlistenFn } from "@tauri-apps/api/event";

interface Props {
  tab: WorkspaceTab;
}

const STATUS_LABEL: Record<TerminalSessionStatus, string> = {
  created: "not started",
  starting: "starting",
  running: "running",
  stopping: "stopping",
  stopped: "stopped",
  failed: "failed",
};

/** Mock-mode hint; the literal command is what the user has to run. */
const NO_BACKEND_HINT = "backend unavailable — run `npm run tauri dev`";

export default function SessionView({ tab }: Props) {
  // `isBackendAvailable` cannot change while the page lives (the Tauri bridge
  // either exists at load or does not), so this is a constant per session.
  const backendAvailable = isBackendAvailable();

  const session = useAppStore((s) => s.sessions[tab.id]);
  const startSession = useAppStore((s) => s.startSession);
  const stopSession = useAppStore((s) => s.stopSession);
  const restartSession = useAppStore((s) => s.restartSession);
  const writeSessionInput = useAppStore((s) => s.writeSessionInput);
  const resizeSession = useAppStore((s) => s.resizeSession);
  const applySessionState = useAppStore((s) => s.applySessionState);

  const terminalRef = useRef<SessionTerminalHandle | null>(null);

  const status: TerminalSessionStatus = session?.status ?? "created";
  const busy = status === "starting" || status === "stopping";
  const live = status === "starting" || status === "running" || status === "stopping";

  const printHint = useCallback((action: string) => {
    terminalRef.current?.writeStatus(`${action}: ${NO_BACKEND_HINT}`);
  }, []);

  /** The terminal exists: say hello when there is no PTY to speak for us. */
  const handleReady = useCallback(
    (handle: SessionTerminalHandle) => {
      if (backendAvailable) {
        return;
      }
      handle.writeln(
        "\x1b[2mAI Coding Workspace · terminal view (no backend attached)\x1b[0m",
      );
      handle.writeStatus(NO_BACKEND_HINT);
      handle.writeStatus(
        "keystrokes are not sent anywhere without the Rust core; this pane only previews the layout",
      );
    },
    [backendAvailable],
  );

  /**
   * Live output + lifecycle events.
   *
   * Keyed on the session id, so subscribing happens once per session rather
   * than on every render (the store's status changes on each transition).
   */
  useEffect(() => {
    if (!backendAvailable || !session?.id) {
      return;
    }
    const sessionId = session.id;
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];

    void (async () => {
      const unlistenOutput = await onSessionOutput(sessionId, (data) => {
        // Straight to xterm: PTY output can arrive thousands of times per
        // second and must not pass through React state.
        terminalRef.current?.write(data);
      });
      const unlistenState = await onSessionState(sessionId, (state) => {
        applySessionState(tab.id, state.status, state.exitCode);
      });
      if (disposed) {
        // The session changed while `listen` was in flight.
        unlistenOutput();
        unlistenState();
        return;
      }
      unlisteners.push(unlistenOutput, unlistenState);
    })();

    return () => {
      disposed = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, [backendAvailable, session?.id, tab.id, applySessionState]);

  // Previous status, so transitions can be narrated once each.
  const previousStatusRef = useRef<TerminalSessionStatus>(status);
  useEffect(() => {
    const previous = previousStatusRef.current;
    if (previous === status) {
      return;
    }
    previousStatusRef.current = status;
    const handle = terminalRef.current;
    if (!handle) {
      return;
    }

    if (status === "running") {
      // The PTY was created at the backend's default size; hand it the real
      // geometry now that the terminal has been fitted, otherwise a TUI wraps
      // at the wrong column until the window is resized.
      const size = handle.size();
      if (size) {
        resizeSession(tab.id, size.cols, size.rows);
      }
      handle.writeStatus("session started");
      return;
    }
    if (status === "stopped") {
      handle.writeStatus("session stopped");
      return;
    }
    if (status === "failed") {
      handle.writeStatus(session?.message ?? "session failed");
    }
  }, [status, session?.message, tab.id, resizeSession]);

  const handleData = useCallback(
    (data: string) => {
      writeSessionInput(tab.id, data);
    },
    [tab.id, writeSessionInput],
  );

  const handleResize = useCallback(
    (cols: number, rows: number) => {
      resizeSession(tab.id, cols, rows);
    },
    [tab.id, resizeSession],
  );

  /**
   * Toolbar copy: the selection when there is one, the whole buffer otherwise
   * (the button's tooltip says so - and copying everything is never destructive,
   * which is why the fallback is offered rather than a disabled button).
   *
   * A refused clipboard is reported in the terminal instead of being swallowed;
   * `SessionTerminal` only returns `false` when nothing reached the clipboard.
   */
  const handleCopy = useCallback(async () => {
    const handle = terminalRef.current;
    if (!handle) {
      return;
    }
    const copied = handle.hasSelection()
      ? await handle.copySelection()
      : await handle.copyAll();
    if (!copied) {
      handle.writeStatus("copy failed: the clipboard is not available");
    }
  }, []);

  const chipClass = `session-chip chip-${status}`;

  return (
    <section className="session-view" aria-label="Agent session">
      <div className="session-toolbar">
        <div className="session-toolbar-actions">
          <button
            type="button"
            className="btn btn-primary"
            // Mock mode keeps every button live so each one can explain itself.
            disabled={backendAvailable && live}
            onClick={() => {
              if (!backendAvailable) {
                printHint("Start");
                return;
              }
              void startSession(tab.id);
            }}
          >
            Start
          </button>
          <button
            type="button"
            className="btn"
            disabled={backendAvailable && !live}
            onClick={() => {
              if (!backendAvailable) {
                printHint("Stop");
                return;
              }
              void stopSession(tab.id);
            }}
          >
            Stop
          </button>
          <button
            type="button"
            className="btn"
            disabled={backendAvailable && busy}
            onClick={() => {
              if (!backendAvailable) {
                printHint("Restart");
                return;
              }
              void restartSession(tab.id);
            }}
          >
            Restart
          </button>
          <button
            type="button"
            className="btn btn-quiet"
            // The clipboard behaviour is explained here because the button is
            // the discoverable half of it: Ctrl+C only copies when something is
            // selected, so a user with no selection needs a way in.
            title="Copy the selected text, or the whole terminal when nothing is selected"
            onClick={() => {
              void handleCopy();
            }}
          >
            Copy
          </button>
          <button
            type="button"
            className="btn btn-quiet"
            onClick={() => {
              terminalRef.current?.clear();
              terminalRef.current?.focus();
            }}
          >
            Clear
          </button>
        </div>

        <div className="session-toolbar-status">
          <span className={chipClass} role="status">
            {STATUS_LABEL[status]}
          </span>
          {session?.message ? (
            <span className="session-message" title={session.message}>
              {session.message}
            </span>
          ) : null}
        </div>
      </div>

      <div className="viewport session-viewport">
        {backendAvailable ? null : (
          <div className="session-viewport-notice">
            <BackendNotice
              message={
                "Sessions are hosted by the Rust core, which is not present in this window. " +
                "Start the desktop app with `npm run tauri dev` to run Claude Code in this tab. " +
                "Until then the buttons and the terminal below stay interactive so the layout can be inspected."
              }
            />
          </div>
        )}
        <SessionTerminal
          ref={terminalRef}
          onReady={handleReady}
          onData={handleData}
          onResize={handleResize}
        />
      </div>
    </section>
  );
}
