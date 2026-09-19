import { useAppStore } from "../stores/useAppStore";
import BackendNotice from "./BackendNotice";

/**
 * Shown when no tab is open.
 *
 * Two ways out, matching the two ways a workspace comes to exist: create a new
 * one, or reopen one that is still configured (spec sections 9, 10). Reopening
 * starts no process - the tab comes back with a stopped session.
 */
export default function EmptyState() {
  const workspaces = useAppStore((s) => s.workspaces);
  const workspacesLoaded = useAppStore((s) => s.workspacesLoaded);
  const workspacesError = useAppStore((s) => s.workspacesError);
  const providers = useAppStore((s) => s.providers);
  const backendStatus = useAppStore((s) => s.backendStatus);
  const openWorkspace = useAppStore((s) => s.openWorkspace);
  const openWorkspaceDialog = useAppStore((s) => s.openWorkspaceDialog);

  const backendMissing = backendStatus === "unavailable";

  return (
    <section className="empty-state">
      <h2>No workspace open</h2>
      <p className="muted">
        A workspace is a local project folder bound to an agent and a provider.
        Open one to start a session with its agent in it.
      </p>

      {backendMissing ? (
        <>
          <BackendNotice />
          <p className="muted">
            The New Workspace dialog is still worth a look: it renders, validates
            and explains itself, but saving needs the Rust core.
          </p>
        </>
      ) : null}

      <div className="empty-state-actions">
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => openWorkspaceDialog({ mode: "create" })}
        >
          + New Workspace
        </button>
      </div>

      {workspacesError !== null && !backendMissing ? (
        <p className="alert alert-error" role="alert">
          {workspacesError}
        </p>
      ) : null}

      {!backendMissing && providers.length === 0 ? (
        <p className="muted">
          A workspace needs a provider profile. Add one in the Providers panel
          first.
        </p>
      ) : null}

      {!backendMissing && workspacesLoaded && workspaces.length > 0 ? (
        <div className="empty-state-list">
          <h3>Configured workspaces</h3>
          <ul className="workspace-list">
            {workspaces.map((workspace) => (
              <li key={workspace.id}>
                <button
                  type="button"
                  className="workspace-list-item"
                  onClick={() => openWorkspace(workspace.id)}
                  title={`Open ${workspace.projectPath}`}
                >
                  <span className="workspace-list-name">{workspace.name}</span>
                  <span className="workspace-list-path mono muted">
                    {workspace.projectPath}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </section>
  );
}
