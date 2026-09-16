import { tabStatusFromSession, useAppStore } from "../stores/useAppStore";

export default function StatusBar() {
  const tabs = useAppStore((s) => s.tabs);
  const sessions = useAppStore((s) => s.sessions);
  const activeTabId = useAppStore((s) => s.activeTabId);
  const workspaces = useAppStore((s) => s.workspaces);
  const providers = useAppStore((s) => s.providers);
  const active = tabs.find((t) => t.id === activeTabId) ?? null;
  const session = active ? sessions[active.id] : undefined;
  // Same mapping the tab dot uses, so the footer and the tab agree.
  const status = active
    ? session
      ? tabStatusFromSession(session.status)
      : "idle"
    : null;

  // Show the provider's display name where one exists; the id is the honest
  // fallback for a workspace whose provider profile was deleted.
  const workspace = active
    ? workspaces.find((entry) => entry.id === active.workspaceId)
    : undefined;
  const providerId = workspace?.providerId ?? active?.provider ?? "";
  const providerLabel =
    providers.find((entry) => entry.id === providerId)?.name ?? providerId;
  const model = workspace?.model ?? (active?.model || "provider default");

  return (
    <footer className="statusbar">
      <span>AI Coding Workspace v0.1.0</span>
      <span className="mono">
        {active && status
          ? `${status} — ${active.title} · ${active.agent} · ${providerLabel}/${model}`
          : "no active session"}
      </span>
    </footer>
  );
}
