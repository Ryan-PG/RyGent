import { useEffect, useState } from "react";
import { useAppStore } from "../stores/useAppStore";
import type {
  ProviderInput,
  ProviderProfile,
  ProviderTestResult,
  SecretStatus,
} from "../types";
import BackendNotice from "./BackendNotice";
import ProviderForm from "./ProviderForm";

/** Which form is open in the panel, if any. */
type Editor =
  | { mode: "create" }
  | { mode: "edit"; provider: ProviderProfile }
  | null;

/** Sentinel for "the create form is saving". */
const NEW_PROVIDER = "new";

/**
 * Provider management (spec section 12).
 *
 * Profiles come from SQLite through the Rust core; API keys live in the OS
 * keyring and are only ever reported back as "set / not set" - the panel never
 * holds a stored key (spec sections 5, 17).
 */
export default function ProvidersPanel() {
  const providers = useAppStore((state) => state.providers);
  const secretStatus = useAppStore((state) => state.secretStatus);
  const providersLoaded = useAppStore((state) => state.providersLoaded);
  const providersError = useAppStore((state) => state.providersError);
  const backendStatus = useAppStore((state) => state.backendStatus);
  const testResults = useAppStore((state) => state.testResults);
  const testingProviderIds = useAppStore((state) => state.testingProviderIds);
  const ensureProvidersLoaded = useAppStore(
    (state) => state.ensureProvidersLoaded,
  );
  const saveProvider = useAppStore((state) => state.saveProvider);
  const deleteProvider = useAppStore((state) => state.deleteProvider);
  const clearProviderSecret = useAppStore((state) => state.clearProviderSecret);
  const testProvider = useAppStore((state) => state.testProvider);
  const clearTestResult = useAppStore((state) => state.clearTestResult);

  const [editor, setEditor] = useState<Editor>(null);
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(
    null,
  );
  // Id of the provider whose save is in flight ("new" for the create form).
  const [savingId, setSavingId] = useState<string | null>(null);

  useEffect(() => {
    ensureProvidersLoaded();
  }, [ensureProvidersLoaded]);

  const backendMissing = backendStatus === "unavailable";

  async function handleSave(
    input: ProviderInput,
    apiKey: string,
    providerId?: string,
  ): Promise<boolean> {
    setSavingId(providerId ?? NEW_PROVIDER);
    try {
      const saved = await saveProvider(input, apiKey, providerId);
      if (saved) {
        setEditor(null);
        setConfirmingDeleteId(null);
      }
      return saved;
    } finally {
      setSavingId(null);
    }
  }

  return (
    <section className="providers-panel" aria-label="Provider management">
      <header className="panel-header">
        <h2>Providers</h2>
        <div className="panel-header-actions">
          <span className="muted">
            {backendMissing
              ? "backend unavailable"
              : providersLoaded
                ? `${providers.length} configured`
                : "loading…"}
          </span>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => setEditor({ mode: "create" })}
            disabled={backendMissing || editor?.mode === "create"}
          >
            + Add provider
          </button>
        </div>
      </header>

      {backendMissing ? (
        <BackendNotice message={providersError ?? undefined} />
      ) : (
        <>
          {providersError !== null ? (
            <p className="alert alert-error" role="alert">
              {providersError}
            </p>
          ) : null}

          {editor?.mode === "create" ? (
            <ProviderForm
              secretStatus={null}
              submitting={savingId === NEW_PROVIDER}
              onCancel={() => setEditor(null)}
              onSubmit={(input, apiKey) => handleSave(input, apiKey)}
              onClearSecret={() => undefined}
            />
          ) : null}

          {!providersLoaded ? <p className="muted">Loading providers…</p> : null}

          {providersLoaded && providers.length === 0 && editor === null ? (
            <div className="provider-empty">
              <p className="muted">
                No providers configured yet. Add a provider profile to give
                Claude Code an API endpoint, a model, and a stored key - the key
                is sent to sessions as a bearer token, not as an API-key header.
              </p>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => setEditor({ mode: "create" })}
              >
                + Add provider
              </button>
            </div>
          ) : null}

          <ul className="provider-list">
            {providers.map((provider) => (
              <ProviderRow
                key={provider.id}
                provider={provider}
                secretStatus={secretStatus[provider.id] ?? null}
                testResult={testResults[provider.id]}
                testing={testingProviderIds.includes(provider.id)}
                saving={savingId === provider.id}
                editing={
                  editor?.mode === "edit" && editor.provider.id === provider.id
                }
                confirmingDelete={confirmingDeleteId === provider.id}
                onEdit={() => setEditor({ mode: "edit", provider })}
                onCancelEdit={() => setEditor(null)}
                onTest={() => void testProvider(provider.id)}
                onDismissTest={() => clearTestResult(provider.id)}
                onRequestDelete={() => setConfirmingDeleteId(provider.id)}
                onCancelDelete={() => setConfirmingDeleteId(null)}
                onConfirmDelete={() => {
                  setConfirmingDeleteId(null);
                  if (
                    editor?.mode === "edit" &&
                    editor.provider.id === provider.id
                  ) {
                    setEditor(null);
                  }
                  void deleteProvider(provider.id);
                }}
                onSave={(input, apiKey) => handleSave(input, apiKey, provider.id)}
                onClearSecret={() => void clearProviderSecret(provider.id)}
              />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

interface RowProps {
  provider: ProviderProfile;
  secretStatus: SecretStatus;
  testResult: ProviderTestResult | undefined;
  testing: boolean;
  saving: boolean;
  editing: boolean;
  confirmingDelete: boolean;
  onEdit: () => void;
  onCancelEdit: () => void;
  onTest: () => void;
  onDismissTest: () => void;
  onRequestDelete: () => void;
  onCancelDelete: () => void;
  onConfirmDelete: () => void;
  onSave: (input: ProviderInput, apiKey: string) => Promise<boolean>;
  onClearSecret: () => void;
}

function ProviderRow({
  provider,
  secretStatus,
  testResult,
  testing,
  saving,
  editing,
  confirmingDelete,
  onEdit,
  onCancelEdit,
  onTest,
  onDismissTest,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete,
  onSave,
  onClearSecret,
}: RowProps) {
  return (
    <li className="provider-card">
      <div className="provider-card-main">
        <div className="provider-card-title">
          <span className="provider-name">{provider.name}</span>
          <SecretBadge status={secretStatus} />
          {provider.maxContextTokens != null ? (
            <ContextWindowBadge tokens={provider.maxContextTokens} />
          ) : null}
        </div>
        <dl className="provider-meta">
          <dt>Base URL</dt>
          <dd className="mono">{provider.baseUrl}</dd>
          <dt>Model</dt>
          <dd className="mono">{provider.model}</dd>
          {provider.extraEnv.length > 0 ? (
            <>
              <dt>Extra env</dt>
              {/* Names only: a user may have put a gateway token in one of
                  these values, and the list view has no need to show it
                  (spec section 17). The edit form shows values because they
                  cannot be edited otherwise. */}
              <dd className="mono">
                {provider.extraEnv.map(([key]) => key).join("  ")}
              </dd>
            </>
          ) : null}
        </dl>
      </div>

      <div className="provider-card-actions">
        <button type="button" className="btn" onClick={onTest} disabled={testing}>
          {testing ? "Testing…" : "Test"}
        </button>
        <button
          type="button"
          className="btn"
          onClick={editing ? onCancelEdit : onEdit}
        >
          {editing ? "Close" : "Edit"}
        </button>
        {confirmingDelete ? (
          <>
            <button
              type="button"
              className="btn btn-danger"
              onClick={onConfirmDelete}
            >
              Confirm delete
            </button>
            <button
              type="button"
              className="btn btn-quiet"
              onClick={onCancelDelete}
            >
              Cancel
            </button>
          </>
        ) : (
          <button
            type="button"
            className="btn btn-danger"
            onClick={onRequestDelete}
          >
            Delete
          </button>
        )}
      </div>

      {testResult !== undefined ? (
        <p
          className={
            "test-result " +
            (testResult.ok ? "test-result-ok" : "test-result-fail")
          }
          role="status"
        >
          <span className="mono">
            {testResult.ok ? "OK" : "FAILED"}
            {testResult.status !== null ? ` · HTTP ${testResult.status}` : ""}
          </span>{" "}
          <span>{testResult.message}</span>{" "}
          <button type="button" className="btn btn-quiet" onClick={onDismissTest}>
            Dismiss
          </button>
        </p>
      ) : null}

      {confirmingDelete ? (
        <p className="alert alert-warn" role="alert">
          Deleting <strong>{provider.name}</strong> also removes its stored API
          key from the OS keyring. Workspaces that reference it are kept.
        </p>
      ) : null}

      {editing ? (
        <ProviderForm
          provider={provider}
          secretStatus={secretStatus}
          submitting={saving}
          onCancel={onCancelEdit}
          onSubmit={onSave}
          onClearSecret={onClearSecret}
        />
      ) : null}
    </li>
  );
}

/** Keyring presence badge: set, not set, or unknown (never the key itself). */
function SecretBadge({ status }: { status: SecretStatus }) {
  if (status === true) {
    return (
      <span
        className="badge badge-ok"
        title="A credential is stored in the OS keyring; sessions send it as a bearer token (ANTHROPIC_AUTH_TOKEN)"
      >
        key set
      </span>
    );
  }
  if (status === false) {
    return (
      <span className="badge" title="No credential stored for this provider">
        no key
      </span>
    );
  }
  return (
    <span className="badge badge-warn" title="The OS keyring could not be read">
      key status unknown
    </span>
  );
}

/**
 * Declared context window, shown only when the user set one - it is a deliberate
 * override of Claude Code's assumed 200k default, so its presence in the list is
 * the confirmation that it took effect.
 */
function ContextWindowBadge({ tokens }: { tokens: number }) {
  return (
    <span
      className="badge"
      title={`Declared context window: ${tokens.toLocaleString("en-US")} tokens, exported to sessions as CLAUDE_CODE_MAX_CONTEXT_TOKENS`}
    >
      <span className="mono">{formatContextWindow(tokens)}</span> ctx
    </span>
  );
}

/** `200k ctx` / `1M ctx` for a round window; the exact count otherwise. */
function formatContextWindow(tokens: number): string {
  if (tokens >= 1_000_000 && tokens % 1_000_000 === 0) {
    return `${tokens / 1_000_000}M`;
  }
  if (tokens >= 1_000 && tokens % 1_000 === 0) {
    return `${tokens / 1_000}k`;
  }
  return tokens.toLocaleString("en-US");
}
