import { useEffect, useState } from "react";
import { useAppStore } from "./stores/useAppStore";
import TabBar from "./components/TabBar";
import WorkspacePanel from "./components/WorkspacePanel";
import WorkspaceDialog from "./components/WorkspaceDialog";
import ProvidersPanel from "./components/ProvidersPanel";
import SettingsPanel from "./components/SettingsPanel";
import EmptyState from "./components/EmptyState";
import StatusBar from "./components/StatusBar";
import { watchSystemTheme } from "./settings/theme";
import type { WorkspaceInput } from "./types";

function App() {
  const tabs = useAppStore((s) => s.tabs);
  const activeTabId = useAppStore((s) => s.activeTabId);
  const activePanel = useAppStore((s) => s.activePanel);
  const activeTab = tabs.find((t) => t.id === activeTabId) ?? null;

  const workspaceDialog = useAppStore((s) => s.workspaceDialog);
  const workspaces = useAppStore((s) => s.workspaces);
  const workspacesError = useAppStore((s) => s.workspacesError);
  const providers = useAppStore((s) => s.providers);
  const ensureWorkspacesLoaded = useAppStore((s) => s.ensureWorkspacesLoaded);
  const ensureProvidersLoaded = useAppStore((s) => s.ensureProvidersLoaded);
  const createWorkspace = useAppStore((s) => s.createWorkspace);
  const updateWorkspace = useAppStore((s) => s.updateWorkspace);
  const closeWorkspaceDialog = useAppStore((s) => s.openWorkspaceDialog);
  const setActivePanel = useAppStore((s) => s.setActivePanel);
  const saveLayout = useAppStore((s) => s.saveLayout);
  const confirmCloseTabId = useAppStore((s) => s.confirmCloseTabId);
  const confirmCloseTab = useAppStore((s) => s.confirmCloseTab);
  const cancelCloseTab = useAppStore((s) => s.cancelCloseTab);
  const [saving, setSaving] = useState(false);

  // Load persisted workspaces and the remembered tab set once, on start
  // (spec section 14). Both are idempotent, so StrictMode's double-invoke is
  // harmless. Nothing is started here: sessions come back stopped.
  //
  // Preferences load as part of this - `initializeWorkspaces` awaits them, since
  // "restore tabs" is itself a preference - so there is no separate call. The
  // Settings panel calls `ensurePreferencesLoaded` for the case where it is
  // opened in a window whose workspace load failed before reaching them.
  useEffect(() => {
    ensureWorkspacesLoaded();
    ensureProvidersLoaded();
  }, [ensureWorkspacesLoaded, ensureProvidersLoaded]);

  // Follow the OS palette while the theme preference is "system". The
  // subscription is permanent and cheap; the store decides whether the change
  // means anything, because the mode can change at any time.
  useEffect(() => watchSystemTheme(() => useAppStore.getState().refreshTheme()), []);

  // Remember which tabs are open whenever that set changes. The store skips the
  // write until the stored layout was read successfully, so a failed load can
  // never overwrite the user's tab set with nothing.
  useEffect(() => {
    void saveLayout();
  }, [tabs, activeTabId, saveLayout]);

  // The edit target can disappear (a second window, or a delete racing the
  // dialog); in that case the dialog is simply not rendered.
  const editingWorkspace =
    workspaceDialog?.mode === "edit"
      ? workspaces.find((entry) => entry.id === workspaceDialog.workspaceId)
      : undefined;
  const dialogOpen =
    workspaceDialog?.mode === "create" ||
    (workspaceDialog?.mode === "edit" && editingWorkspace !== undefined);

  // The tab awaiting a close confirmation. Found from the live tab list rather
  // than stored, so a tab that disappeared another way (a workspace deleted from
  // a second window) cannot leave a dialog asking about nothing.
  const tabPendingClose =
    confirmCloseTabId !== null
      ? (tabs.find((tab) => tab.id === confirmCloseTabId) ?? null)
      : null;

  async function handleSubmit(input: WorkspaceInput): Promise<boolean> {
    setSaving(true);
    try {
      return editingWorkspace
        ? await updateWorkspace(editingWorkspace.id, input)
        : await createWorkspace(input);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="app-shell">
      <TabBar />
      <main className="app-main">
        {activePanel === "providers" ? (
          <ProvidersPanel />
        ) : activePanel === "settings" ? (
          <SettingsPanel />
        ) : activeTab ? (
          <WorkspacePanel tab={activeTab} />
        ) : (
          <EmptyState />
        )}
      </main>
      <StatusBar />

      {dialogOpen ? (
        <WorkspaceDialog
          workspace={editingWorkspace}
          providers={providers}
          submitting={saving}
          error={workspacesError}
          onCancel={() => closeWorkspaceDialog(null)}
          onSubmit={handleSubmit}
          onOpenProviders={() => {
            closeWorkspaceDialog(null);
            setActivePanel("providers");
          }}
        />
      ) : null}

      {tabPendingClose !== null ? (
        <div
          className="modal-backdrop"
          role="presentation"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              cancelCloseTab();
            }
          }}
        >
          <div
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-label="Close running session"
          >
            <h3>Close {tabPendingClose.title}?</h3>
            <p className="muted">
              Its agent is still running. Closing the tab stops that session
              immediately. The workspace stays configured and can be reopened,
              but anything the agent was in the middle of is lost.
            </p>
            <p className="field-hint muted">
              Turn this prompt off in Settings → Sessions.
            </p>
            <div className="form-actions">
              <button
                type="button"
                className="btn btn-danger"
                onClick={confirmCloseTab}
              >
                Close tab
              </button>
              <button type="button" className="btn" onClick={cancelCloseTab} autoFocus>
                Keep it open
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

export default App;
