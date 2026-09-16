import { useEffect, useState } from "react";
import { useAppStore } from "./stores/useAppStore";
import TabBar from "./components/TabBar";
import WorkspacePanel from "./components/WorkspacePanel";
import WorkspaceDialog from "./components/WorkspaceDialog";
import ProvidersPanel from "./components/ProvidersPanel";
import SettingsPanel from "./components/SettingsPanel";
import EmptyState from "./components/EmptyState";
import StatusBar from "./components/StatusBar";
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
  const [saving, setSaving] = useState(false);

  // Load persisted workspaces and the remembered tab set once, on start
  // (spec section 14). Both are idempotent, so StrictMode's double-invoke is
  // harmless. Nothing is started here: sessions come back stopped.
  useEffect(() => {
    ensureWorkspacesLoaded();
    ensureProvidersLoaded();
  }, [ensureWorkspacesLoaded, ensureProvidersLoaded]);

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
    </div>
  );
}

export default App;
