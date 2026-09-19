/**
 * Create / edit a workspace (spec section 12, "New Workspace").
 *
 * Fields: project folder (with the native picker), agent (every agent the
 * backend reports, spec section 24), provider (from the configured profiles),
 * model (defaults to the chosen provider's model and stays editable) and name.
 *
 * The agent list comes from `list_agents`, so this form never names an agent:
 * a new adapter in the Rust core appears here with no change to this file. An
 * agent that is not installed can still be saved - the workspace is a
 * declaration, and installing the CLI later should not require retyping it -
 * but the dialog says so, and starting a session explains the failure again
 * from the backend's own discovery result.
 *
 * The folder field is always editable, and the picker is treated as a
 * convenience: if it is unavailable - a plain browser, or a window where the
 * plugin call fails - the dialog says why and the path can still be typed. No
 * part of this form is allowed to *require* a native dialog.
 *
 * Secrets are not involved at all: a workspace references a provider profile by
 * id, and the API key stays in the OS keyring (spec section 17).
 */

import { useEffect, useState, type FormEvent } from "react";
import { pickProjectFolder, type FolderPickerOutcome } from "../services/workspaces";
import {
  AGENT_ID,
  agentOptions,
  useAppStore,
} from "../stores/useAppStore";
import { preferences } from "../settings/preferences";
import type { AgentInfo, ProviderProfile, Workspace, WorkspaceInput } from "../types";

interface Props {
  /** Workspace being edited; omitted when creating a new one. */
  workspace?: Workspace;
  /** Configured provider profiles (metadata only - never a key). */
  providers: ProviderProfile[];
  /** A save is in flight. */
  submitting: boolean;
  /** Backend error from the last attempt. */
  error: string | null;
  onCancel: () => void;
  /** Returns `true` when the backend accepted the save. */
  onSubmit: (input: WorkspaceInput) => Promise<boolean>;
  /** Jump to the Providers panel (shown when there is nothing to select). */
  onOpenProviders: () => void;
}

/**
 * Pick the provider profile to preselect in the form.
 *
 * Precedence: the workspace's own provider when editing, then the configured
 * default from Settings, then the first configured profile - which is what this
 * dialog did before the setting existed. A default that no longer resolves (its
 * profile was deleted) falls through rather than leaving the form on a provider
 * that is not in the list.
 *
 * Secrets are not involved: this is a profile **id**.
 */
function defaultProvider(
  workspace: Workspace | undefined,
  providers: ProviderProfile[],
  configuredDefaultId: string,
): string {
  if (
    workspace &&
    providers.some((provider) => provider.id === workspace.providerId)
  ) {
    return workspace.providerId;
  }
  if (
    configuredDefaultId !== "" &&
    providers.some((provider) => provider.id === configuredDefaultId)
  ) {
    return configuredDefaultId;
  }
  return providers[0]?.id ?? "";
}

/**
 * The agent options this form offers.
 *
 * The backend's list is the source of truth. One case needs adding to: a
 * persisted workspace naming an agent this build no longer lists (an older
 * release, or an adapter removed from the registry). Dropping the option would
 * leave the select blank and silently rewrite the workspace to a different
 * agent on the next save, so the value is shown as-is and only replaced when
 * the user picks something else.
 */
function agentChoices(agents: AgentInfo[], currentId: string): AgentInfo[] {
  const options = agentOptions(agents);
  if (currentId === "" || options.some((agent) => agent.id === currentId)) {
    return options;
  }
  return [
    ...options,
    {
      id: currentId,
      name: `${currentId} (not in this build)`,
      installed: null,
      executablePath: null,
    },
  ];
}

export default function WorkspaceDialog({
  workspace,
  providers,
  submitting,
  error,
  onCancel,
  onSubmit,
  onOpenProviders,
}: Props) {
  const isEditing = workspace !== undefined;
  // Read once through the preference module, so the dialog does not know how
  // the default is stored - only what it resolves to.
  const configuredDefaultId = useAppStore((state) =>
    preferences.workspacesDefaultProvider.get(state.preferences),
  );
  const agents = useAppStore((state) => state.agents);
  // The dialog is the surface that needs the list, so it asks for it rather than
  // assuming startup fetched it - the same pattern the Providers panel uses. In
  // a plain browser this resolves to the built-in names.
  const ensureAgentsLoaded = useAppStore((state) => state.ensureAgentsLoaded);
  const [name, setName] = useState(workspace?.name ?? "");
  const [projectPath, setProjectPath] = useState(workspace?.projectPath ?? "");
  const [agentId, setAgentId] = useState(workspace?.agentId ?? AGENT_ID);
  const [providerId, setProviderId] = useState(() =>
    defaultProvider(workspace, providers, configuredDefaultId),
  );
  const [model, setModel] = useState(
    () =>
      // An explicit per-workspace override wins; otherwise the form starts on
      // the provider's own model, which is what the backend would use anyway.
      workspace?.model ??
      providers.find(
        (p) => p.id === defaultProvider(workspace, providers, configuredDefaultId),
      )?.model ??
      "",
  );
  const [pickerNote, setPickerNote] = useState<string | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);

  // Escape closes the dialog, like every other modal in a desktop app.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onCancel();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onCancel]);

  // `ensureAgentsLoaded` is a no-op once the list is in (startup loads it too),
  // so this costs one guard check in the ordinary case.
  useEffect(() => {
    ensureAgentsLoaded();
  }, [ensureAgentsLoaded]);

  const selectedProvider = providers.find((p) => p.id === providerId);
  const choices = agentChoices(agents, agentId);
  const selectedAgent = choices.find((agent) => agent.id === agentId);
  // "Not looked for" yields no hint: only the backend's own `false` - a
  // completed `PATH` search that found nothing - is worth acting on.
  const agentMissing = selectedAgent?.installed === false;

  function handleProviderChange(nextId: string) {
    setProviderId(nextId);
    // Changing the provider moves the model to that provider's default: a model
    // id from another endpoint is almost never valid, and the field stays
    // editable for the cases where it is.
    setModel(providers.find((p) => p.id === nextId)?.model ?? "");
  }

  async function handleBrowse() {
    const outcome: FolderPickerOutcome = await pickProjectFolder();
    if (outcome.status === "picked") {
      setProjectPath(outcome.path);
      setPickerNote(null);
      return;
    }
    if (outcome.status === "unavailable") {
      setPickerNote(outcome.reason);
    }
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const problem = validate(name, projectPath, agentId, providerId, providers.length);
    if (problem !== null) {
      setLocalError(problem);
      return;
    }
    setLocalError(null);
    // The store closes the dialog on success, so a `true` needs no handling
    // here beyond leaving the field values alone.
    await onSubmit({
      name: name.trim(),
      projectPath: projectPath.trim(),
      agentId,
      providerId,
      // Blank means "use the provider's default model".
      model: model.trim() === "" ? null : model.trim(),
    });
  }

  const shownError = localError ?? error;
  const noProviders = providers.length === 0;

  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onCancel();
        }
      }}
    >
      <form
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={isEditing ? "Edit workspace" : "New workspace"}
        onSubmit={handleSubmit}
        noValidate
      >
        <h3>{isEditing ? `Edit ${workspace.name}` : "New workspace"}</h3>

        <div className="form-grid">
          <label className="field">
            <span className="field-label">Name</span>
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Project Alpha"
              autoFocus
            />
          </label>

          <div className="field">
            <label className="field-label" htmlFor="workspace-project-path">
              Project folder
            </label>
            <div className="field-row">
              <input
                id="workspace-project-path"
                value={projectPath}
                onChange={(event) => setProjectPath(event.target.value)}
                placeholder="D:\\Projects\\project-alpha"
                spellCheck={false}
                className="mono"
              />
              <button type="button" className="btn" onClick={() => void handleBrowse()}>
                Browse…
              </button>
            </div>
            <p className="field-hint muted">
              The folder must already exist. It becomes the agent's working
              directory; the application never creates or deletes anything in it.
            </p>
            {pickerNote !== null ? (
              <p className="field-hint muted">{pickerNote}</p>
            ) : null}
          </div>

          <label className="field">
            <span className="field-label">Agent</span>
            <select
              value={agentId}
              onChange={(event) => setAgentId(event.target.value)}
            >
              {choices.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.name}
                </option>
              ))}
            </select>
            <p className="field-hint muted">
              The CLI this workspace runs. Each session gets its own config
              directory, so two agents - or two sessions - never share state.
            </p>
          </label>

          <label className="field">
            <span className="field-label">Provider</span>
            <select
              value={providerId}
              onChange={(event) => handleProviderChange(event.target.value)}
              disabled={noProviders}
            >
              {noProviders ? <option value="">no providers configured</option> : null}
              {providers.map((provider) => (
                <option key={provider.id} value={provider.id}>
                  {provider.name}
                </option>
              ))}
            </select>
            <p className="field-hint muted">
              Supplies this session's API endpoint, model and credential. The
              credential itself stays in the OS keyring.
            </p>
          </label>

          <label className="field">
            <span className="field-label">Model</span>
            <input
              value={model}
              onChange={(event) => setModel(event.target.value)}
              placeholder={selectedProvider?.model ?? "model-a"}
              spellCheck={false}
              className="mono"
              disabled={noProviders}
            />
            <p className="field-hint muted">
              Defaults to the provider's model. Clear the field to use the
              provider default at launch time.
            </p>
          </label>
        </div>

        {agentMissing ? (
          <p className="alert alert-warn" role="alert">
            {selectedAgent.name} was not found on this machine's PATH. The
            workspace can still be saved; install the CLI before starting it, or
            a session will fail with the same message.
          </p>
        ) : null}

        {noProviders ? (
          <p className="alert alert-warn" role="alert">
            No provider profiles yet, so a workspace cannot be created.{" "}
            <button type="button" className="btn btn-quiet" onClick={onOpenProviders}>
              Go to Providers
            </button>
          </p>
        ) : null}

        {shownError !== null ? (
          <p className="alert alert-error" role="alert">
            {shownError}
          </p>
        ) : null}

        <div className="form-actions">
          <button type="submit" className="btn btn-primary" disabled={submitting || noProviders}>
            {submitting
              ? "Saving…"
              : isEditing
                ? "Save changes"
                : "Create workspace"}
          </button>
          <button type="button" className="btn" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}

/**
 * Client-side validation mirroring `WorkspaceManager::validate` in
 * `src-tauri/src/workspaces/mod.rs`. The backend validates again - this exists to
 * fail fast with a clear message, not to replace it.
 */
function validate(
  name: string,
  projectPath: string,
  agentId: string,
  providerId: string,
  providerCount: number,
): string | null {
  if (name.trim() === "") {
    return "Name is required.";
  }
  if (projectPath.trim() === "") {
    return "Project folder is required.";
  }
  if (agentId.trim() === "") {
    return "Select an agent.";
  }
  if (providerCount === 0) {
    return "Add a provider profile before creating a workspace.";
  }
  if (providerId === "") {
    return "Select a provider.";
  }
  return null;
}
