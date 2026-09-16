import { useState } from "react";
import SessionView from "./SessionView";
import { useAppStore } from "../stores/useAppStore";
import type { WorkspaceTab } from "../types";

interface Props {
  tab: WorkspaceTab;
}

/**
 * The active workspace: its real configuration (spec section 12) plus the agent
 * session view.
 *
 * Everything shown here comes from the persisted workspace row and the provider
 * list, not from the tab's denormalized copy - only the tab identity is taken
 * from the tab. Secrets are never involved: the provider is shown by name, and
 * the API key stays in the OS keyring.
 */
export default function WorkspacePanel({ tab }: Props) {
  // A stable object reference from the list, so this does not re-render on every
  // store change.
  const workspace = useAppStore((state) =>
    state.workspaces.find((entry) => entry.id === tab.workspaceId),
  );
  const providers = useAppStore((state) => state.providers);
  const openWorkspaceDialog = useAppStore((state) => state.openWorkspaceDialog);
  const deleteWorkspace = useAppStore((state) => state.deleteWorkspace);
  const closeTab = useAppStore((state) => state.closeTab);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  const name = workspace?.name ?? tab.title;
  const projectPath = workspace?.projectPath ?? tab.projectPath;
  const providerId = workspace?.providerId ?? tab.provider;
  const provider = providers.find((entry) => entry.id === providerId);
  const model = workspace?.model ?? null;

  return (
    <section className="workspace-panel" aria-label="Workspace details">
      <header className="workspace-header">
        <h2 className="workspace-title">{name}</h2>
        <div className="workspace-actions">
          <button
            type="button"
            className="btn"
            disabled={workspace === undefined}
            onClick={() =>
              openWorkspaceDialog({ mode: "edit", workspaceId: tab.workspaceId })
            }
          >
            Edit
          </button>
          {confirmingDelete ? (
            <>
              <button
                type="button"
                className="btn btn-danger"
                onClick={() => {
                  setConfirmingDelete(false);
                  void deleteWorkspace(tab.workspaceId);
                }}
              >
                Confirm remove
              </button>
              <button
                type="button"
                className="btn btn-quiet"
                onClick={() => setConfirmingDelete(false)}
              >
                Cancel
              </button>
            </>
          ) : (
            <button
              type="button"
              className="btn btn-danger"
              disabled={workspace === undefined}
              onClick={() => setConfirmingDelete(true)}
            >
              Remove
            </button>
          )}
          <button
            type="button"
            className="btn btn-quiet"
            title="Close the tab. The workspace stays configured and can be reopened."
            onClick={() => closeTab(tab.id)}
          >
            Close tab
          </button>
        </div>
      </header>

      <dl className="meta-grid">
        <dt>Project path</dt>
        <dd className="mono">{projectPath}</dd>

        <dt>Agent</dt>
        <dd>{tab.agent}</dd>

        <dt>Provider</dt>
        <dd className="mono">
          {provider ? (
            provider.name
          ) : (
            <>
              {providerId}{" "}
              <span className="badge badge-warn" title="This provider profile no longer exists. Edit the workspace to pick another one.">
                missing
              </span>
            </>
          )}
        </dd>

        <dt>Model</dt>
        <dd className="mono">
          {model ?? <span className="muted">provider default</span>}
        </dd>
      </dl>

      {confirmingDelete ? (
        <p className="alert alert-warn" role="alert">
          This removes <strong>{name}</strong> from the application and closes its
          tab. Your project folder and every file in it are left untouched, and so
          are the provider profile and its stored API key. You can add the folder
          again later.
        </p>
      ) : null}

      {/* Keyed by tab id so each workspace gets its own terminal (and therefore
          its own xterm instance, scrollback and PTY) rather than sharing one
          that would be reused across tabs. Switching tabs remounts a different
          terminal and leaves every other session running. */}
      <SessionView key={tab.id} tab={tab} />
    </section>
  );
}
