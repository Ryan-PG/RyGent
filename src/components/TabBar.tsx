import { useRef, useState } from "react";
import {
  tabStatusFromSession,
  useAppStore,
  type Panel,
} from "../stores/useAppStore";
import type { SessionStatus } from "../types";
import WorkspaceSwitcher from "./WorkspaceSwitcher";

const STATUS_LABEL: Record<SessionStatus, string> = {
  idle: "idle",
  running: "running",
  exited: "exited",
  failed: "failed",
};

const PANELS: { id: Panel; label: string }[] = [
  { id: "workspaces", label: "Workspace" },
  { id: "providers", label: "Providers" },
  { id: "settings", label: "Settings" },
];

export default function TabBar() {
  const tabs = useAppStore((s) => s.tabs);
  const sessions = useAppStore((s) => s.sessions);
  const activeTabId = useAppStore((s) => s.activeTabId);
  // `requestCloseTab` rather than `closeTab`: closing a tab with a live session
  // asks first, when "confirm before closing a running session" is on.
  const requestCloseTab = useAppStore((s) => s.requestCloseTab);
  const setActiveTab = useAppStore((s) => s.setActiveTab);
  const activePanel = useAppStore((s) => s.activePanel);
  const setActivePanel = useAppStore((s) => s.setActivePanel);
  const openWorkspaceDialog = useAppStore((s) => s.openWorkspaceDialog);
  const [switcherOpen, setSwitcherOpen] = useState(false);
  // The switcher's popover is portaled to `<body>` and placed from this
  // button's viewport rect: it cannot be anchored in CSS here, because the tab
  // strip scrolls and clips anything positioned inside it.
  const openButtonRef = useRef<HTMLButtonElement | null>(null);

  return (
    <header className="tabbar">
      <div className="tabbar-tabs" role="tablist" aria-label="Workspaces">
        {tabs.map((tab) => {
          // The dot reflects the real session state machine; a tab whose agent
          // was never started has no session entry and reads "idle".
          const session = sessions[tab.id];
          const status = session ? tabStatusFromSession(session.status) : "idle";
          return (
            <div
              key={tab.id}
              role="tab"
              tabIndex={0}
              aria-selected={tab.id === activeTabId}
              className={"tab" + (tab.id === activeTabId ? " tab-active" : "")}
              onClick={() => setActiveTab(tab.id)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setActiveTab(tab.id);
                }
              }}
              title={`${tab.title} — ${STATUS_LABEL[status]}\n${tab.projectPath}`}
            >
              <span className={`status-dot dot-${status}`} aria-label={status} />
              <span className="tab-title">{tab.title}</span>
              <button
                type="button"
                className="tab-close"
                aria-label={`Close ${tab.title}`}
                title="Close tab (the workspace stays configured)"
                onClick={(e) => {
                  e.stopPropagation();
                  requestCloseTab(tab.id);
                }}
              >
                ×
              </button>
            </div>
          );
        })}

        <div className="tabbar-new">
          <button
            type="button"
            className="btn new-workspace"
            onClick={() => openWorkspaceDialog({ mode: "create" })}
          >
            + New Workspace
          </button>
          <button
            ref={openButtonRef}
            type="button"
            className="btn btn-quiet open-workspace"
            aria-expanded={switcherOpen}
            aria-haspopup="menu"
            onClick={() => setSwitcherOpen((open) => !open)}
            title="Reopen a configured workspace"
          >
            Open ▾
          </button>
          {switcherOpen ? (
            <WorkspaceSwitcher
              anchorRef={openButtonRef}
              onClose={() => setSwitcherOpen(false)}
            />
          ) : null}
        </div>
      </div>

      <nav className="panel-switcher" aria-label="Panel">
        {PANELS.map((p) => (
          <button
            key={p.id}
            type="button"
            className={
              "btn panel-btn" + (activePanel === p.id ? " panel-btn-active" : "")
            }
            aria-pressed={activePanel === p.id}
            onClick={() => setActivePanel(p.id)}
          >
            {p.label}
          </button>
        ))}
      </nav>
    </header>
  );
}
