/**
 * Typed wrappers over the workspace commands (spec sections 9, 10, 12, 14).
 *
 * Command names and argument shapes mirror
 * `src-tauri/src/commands/workspaces.rs`. Nothing here carries a credential: a
 * workspace references a provider by id, and the API key never leaves the Rust
 * core (`services/providers.ts` can only ask whether one exists).
 *
 * `pickProjectFolder` is the one non-command function here: the folder picker is
 * `tauri-plugin-dialog`'s native dialog, opened from this side of the boundary.
 * If it is unavailable (a plain browser, or a window where the plugin call
 * fails) it resolves to `null` and the dialog keeps its editable path field as
 * the manual fallback - the app must never *require* the picker.
 */

import { call, isBackendAvailable } from "./backend";
import type {
  Workspace,
  WorkspaceInput,
  WorkspaceLayout,
} from "../types";

export const workspacesApi = {
  /** Every configured workspace, in creation order. */
  list: (): Promise<Workspace[]> => call<Workspace[]>("list_workspaces"),

  /** Create a workspace. Validation happens in the Rust core. */
  create: (input: WorkspaceInput): Promise<Workspace> =>
    call<Workspace>("create_workspace", { input }),

  /** Rename a workspace or repoint its folder/agent/provider/model. */
  update: (id: string, input: WorkspaceInput): Promise<Workspace> =>
    call<Workspace>("update_workspace", { id, input }),

  /** Remove a workspace's configuration (never its project files). */
  remove: (id: string): Promise<void> => call<void>("delete_workspace", { id }),

  /** The tab set that was open when the app last closed. */
  loadLayout: (): Promise<WorkspaceLayout> =>
    call<WorkspaceLayout>("load_workspace_layout"),

  /** Remember the open tab set. Returns the normalized value that was stored. */
  saveLayout: (layout: WorkspaceLayout): Promise<WorkspaceLayout> =>
    call<WorkspaceLayout>("save_workspace_layout", { layout }),
};

/** Why a folder-picker call produced no path. */
export type FolderPickerOutcome =
  | { status: "picked"; path: string }
  /** The user closed the dialog. */
  | { status: "cancelled" }
  /** No native picker here: the caller should fall back to typing a path. */
  | { status: "unavailable"; reason: string };

/**
 * Open the native folder picker (spec section 12: "Project folder").
 *
 * Never throws: every failure is reported as `unavailable` so the New Workspace
 * dialog can say what happened and let the user type the path instead. The
 * plugin is imported dynamically so a browser-only build does not pay for it.
 */
export async function pickProjectFolder(): Promise<FolderPickerOutcome> {
  if (!isBackendAvailable()) {
    return {
      status: "unavailable",
      reason:
        "the desktop folder picker needs the Rust core - start the app with `npm run tauri dev`, or type the path",
    };
  }
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select the project folder",
    });
    if (typeof selected !== "string") {
      // `null` when dismissed; an array only if `multiple` were set.
      return { status: "cancelled" };
    }
    return { status: "picked", path: selected };
  } catch (error) {
    return {
      status: "unavailable",
      reason: `the folder picker could not be opened (${
        error instanceof Error ? error.message : String(error)
      }) - type the path instead`,
    };
  }
}
