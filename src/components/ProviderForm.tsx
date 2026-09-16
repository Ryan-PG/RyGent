import { useState, type FormEvent } from "react";
import type {
  EnvironmentPair,
  ProviderInput,
  ProviderProfile,
  SecretStatus,
} from "../types";

interface EnvRow {
  key: string;
  value: string;
}

/**
 * Smallest declared context window the backend accepts (`MIN_MAX_CONTEXT_TOKENS`
 * in `src-tauri/src/providers/mod.rs`). Kept in sync by hand: the backend still
 * validates, this only fails fast with a readable message.
 */
const MIN_MAX_CONTEXT_TOKENS = 1_000;

/**
 * How the stored credential is actually presented to a session. The app sends
 * it as a bearer token and never as an `X-Api-Key` header, so the helper text
 * next to the field must not imply otherwise.
 */
const BEARER_HINT =
  "Sessions present it as a bearer token (ANTHROPIC_AUTH_TOKEN), not as an X-Api-Key header.";

interface Props {
  /** Provider being edited; omitted when creating a new one. */
  provider?: ProviderProfile;
  /** Whether a key is already stored in the keyring (edit mode). */
  secretStatus: SecretStatus;
  /** A save is in flight. */
  submitting: boolean;
  onCancel: () => void;
  /** Returns `true` when the backend accepted the save. */
  onSubmit: (input: ProviderInput, apiKey: string) => Promise<boolean>;
  /** Delete the stored keyring entry (edit mode only). */
  onClearSecret: () => void;
}

/**
 * Create/edit form for a provider profile (spec section 12).
 *
 * Secrets: the credential field starts empty, is never pre-filled from the
 * stored provider (the backend never returns it), and is cleared from React
 * state as soon as it has been handed to the keyring. On edit, leaving it blank
 * keeps the existing credential (spec sections 5, 17). The stored value reaches
 * the session as a bearer token (`ANTHROPIC_AUTH_TOKEN`); the app never sends it
 * as an `x-api-key` header.
 */
export default function ProviderForm({
  provider,
  secretStatus,
  submitting,
  onCancel,
  onSubmit,
  onClearSecret,
}: Props) {
  const isEditing = provider !== undefined;

  const [name, setName] = useState(provider?.name ?? "");
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? "");
  const [model, setModel] = useState(provider?.model ?? "");
  // Held as text so "empty" stays distinguishable from 0 and from a partly
  // typed number; `null` in the payload means "Claude Code's own default".
  const [maxContextTokens, setMaxContextTokens] = useState(
    provider?.maxContextTokens != null ? String(provider.maxContextTokens) : "",
  );
  const [apiKey, setApiKey] = useState("");
  const [revealKey, setRevealKey] = useState(false);
  const [envRows, setEnvRows] = useState<EnvRow[]>(
    provider?.extraEnv.map(([key, value]) => ({ key, value })) ?? [],
  );
  const [error, setError] = useState<string | null>(null);

  const keyHint = !isEditing
    ? `Stored in the OS keyring (Windows Credential Manager / macOS Keychain / Linux Secret Service) - never in the app database. ${BEARER_HINT}`
    : secretStatus === true
      ? `A key is already stored. Leave this blank to keep it, or type a new key to replace it. ${BEARER_HINT}`
      : secretStatus === false
        ? "No key stored for this provider yet."
        : "The OS keyring could not be read, so the stored key status is unknown.";

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    const pairs = toPairs(envRows);
    const problem = validate(name, baseUrl, model, maxContextTokens, pairs);
    if (problem !== null) {
      setError(problem);
      return;
    }

    setError(null);
    const saved = await onSubmit(
      {
        name: name.trim(),
        baseUrl: baseUrl.trim(),
        model: model.trim(),
        extraEnv: pairs,
        // Always sent, even when empty: the backend reads an absent field as
        // `None` and would clear a window the user declared earlier.
        maxContextTokens: parseMaxContextTokens(maxContextTokens),
      },
      apiKey,
    );

    if (saved) {
      // Never keep the key in UI state after it reached the keyring.
      setApiKey("");
    }
  }

  return (
    <form className="provider-form" onSubmit={handleSubmit} noValidate>
      <h3>
        {provider === undefined ? "New provider" : `Edit ${provider.name}`}
      </h3>

      <div className="form-grid">
        <label className="field">
          <span className="field-label">Name</span>
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Free Provider A"
            autoFocus
          />
        </label>

        <label className="field">
          <span className="field-label">Base URL</span>
          <input
            value={baseUrl}
            onChange={(event) => setBaseUrl(event.target.value)}
            placeholder="https://provider-a.example.com"
            spellCheck={false}
            className="mono"
          />
        </label>

        <label className="field">
          <span className="field-label">Model</span>
          <input
            value={model}
            onChange={(event) => setModel(event.target.value)}
            placeholder="model-a"
            spellCheck={false}
            className="mono"
          />
        </label>

        <div className="field">
          <label className="field-label" htmlFor="provider-max-context-tokens">
            Max context tokens
          </label>
          <input
            id="provider-max-context-tokens"
            type="number"
            inputMode="numeric"
            min={MIN_MAX_CONTEXT_TOKENS}
            value={maxContextTokens}
            onChange={(event) => setMaxContextTokens(event.target.value)}
            placeholder="200000"
            spellCheck={false}
            className="mono"
          />
          <p className="field-hint muted">
            Optional. Leave empty and Claude Code uses its own default window.
            Set the model's real window (for example <code>200000</code>) to
            silence the "model isn't in this version's model catalog" warning and
            size auto-compact correctly. Exported to the session as{" "}
            <code>CLAUDE_CODE_MAX_CONTEXT_TOKENS</code>.
          </p>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="provider-api-key">
            API key
          </label>
          <div className="field-row">
            <input
              id="provider-api-key"
              type={revealKey ? "text" : "password"}
              value={apiKey}
              onChange={(event) => setApiKey(event.target.value)}
              // Password managers and autofill must not stash a provider key.
              autoComplete="new-password"
              spellCheck={false}
              className="mono"
              placeholder={isEditing ? "leave blank to keep the stored key" : "sk-..."}
            />
            <button
              type="button"
              className="btn btn-quiet"
              onClick={() => setRevealKey((current) => !current)}
              aria-pressed={revealKey}
            >
              {revealKey ? "Hide" : "Show"}
            </button>
          </div>
          <p className="field-hint muted">{keyHint}</p>
        </div>
      </div>

      <fieldset className="env-fieldset">
        <legend>Extra environment variables</legend>
        <p className="field-hint muted">
          Added to the session environment after the defaults, so they win on a
          duplicate name. The stored key already reaches the session as a bearer
          token (<code>ANTHROPIC_AUTH_TOKEN</code>); use this for gateway-specific
          variables such as <code>ANTHROPIC_API_KEY</code> (for a gateway that
          requires an <code>X-Api-Key</code> header instead) or{" "}
          <code>ENABLE_TOOL_SEARCH</code>.
        </p>

        {envRows.length === 0 ? (
          <p className="muted">No extra variables.</p>
        ) : (
          <ul className="env-rows">
            {envRows.map((row, index) => (
              <li key={index} className="env-row">
                <input
                  value={row.key}
                  onChange={(event) =>
                    updateRow(envRows, setEnvRows, index, { key: event.target.value })
                  }
                  placeholder="VARIABLE_NAME"
                  aria-label={`Environment variable ${index + 1} name`}
                  spellCheck={false}
                  className="mono"
                />
                <input
                  value={row.value}
                  onChange={(event) =>
                    updateRow(envRows, setEnvRows, index, { value: event.target.value })
                  }
                  placeholder="value"
                  aria-label={`Environment variable ${index + 1} value`}
                  spellCheck={false}
                  className="mono"
                />
                <button
                  type="button"
                  className="btn btn-quiet"
                  onClick={() =>
                    setEnvRows((rows) => rows.filter((_row, i) => i !== index))
                  }
                  aria-label={`Remove environment variable ${index + 1}`}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}

        <button
          type="button"
          className="btn"
          onClick={() => setEnvRows((rows) => [...rows, { key: "", value: "" }])}
        >
          + Add variable
        </button>
      </fieldset>

      {error !== null ? (
        <p className="alert alert-error" role="alert">
          {error}
        </p>
      ) : null}

      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={submitting}>
          {submitting ? "Saving…" : isEditing ? "Save changes" : "Create provider"}
        </button>
        <button type="button" className="btn" onClick={onCancel}>
          Cancel
        </button>
        {isEditing && secretStatus === true ? (
          <button type="button" className="btn btn-danger" onClick={onClearSecret}>
            Remove stored key
          </button>
        ) : null}
      </div>
    </form>
  );
}

/** Apply a partial change to one environment-variable row. */
function updateRow(
  rows: EnvRow[],
  setRows: (rows: EnvRow[]) => void,
  index: number,
  patch: Partial<EnvRow>,
): void {
  setRows(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
}

/** Drop blank rows and convert to the backend's `[name, value]` pairs. */
function toPairs(rows: EnvRow[]): EnvironmentPair[] {
  return rows
    .filter((row) => row.key.trim() !== "" || row.value.trim() !== "")
    .map((row) => [row.key.trim(), row.value]);
}

/**
 * Read the optional declared context window.
 *
 * Empty means "not declared" (`null`, never `0`, which the backend rejects: no
 * real model has a zero-token window). Anything that is not a whole number of
 * tokens, or is below [`MIN_MAX_CONTEXT_TOKENS`], returns `null` here too - but
 * [`validate`] rejects that input first, so this is never reached with a bad
 * value.
 */
function parseMaxContextTokens(raw: string): number | null {
  const text = raw.trim();
  if (!/^\d+$/.test(text)) {
    return null;
  }
  const value = Number(text);
  if (!Number.isSafeInteger(value) || value < MIN_MAX_CONTEXT_TOKENS) {
    return null;
  }
  return value;
}

/** Why a declared context window is unusable, or `null` when it is fine. */
function validateMaxContextTokens(raw: string): string | null {
  const text = raw.trim();
  if (text === "") {
    return null;
  }
  if (!/^\d+$/.test(text)) {
    return "Max context tokens must be a whole number of tokens, for example 200000.";
  }
  const value = Number(text);
  if (!Number.isSafeInteger(value)) {
    return "Max context tokens is larger than this field can represent - enter the model's window in tokens, for example 200000.";
  }
  if (value < MIN_MAX_CONTEXT_TOKENS) {
    return `Max context tokens must be at least ${MIN_MAX_CONTEXT_TOKENS} (a model's real context window), got ${value} - leave the field empty unless Claude Code reports the model as unknown to its catalog.`;
  }
  return null;
}

/**
 * Client-side validation mirroring `ProviderInput::validate` in
 * `src-tauri/src/providers/mod.rs`. The backend validates again - this exists to
 * fail fast with a clear message, not to replace it.
 */
function validate(
  name: string,
  baseUrl: string,
  model: string,
  maxContextTokens: string,
  pairs: EnvironmentPair[],
): string | null {
  if (name.trim() === "") {
    return "Name is required.";
  }

  const url = baseUrl.trim();
  if (url === "") {
    return "Base URL is required.";
  }
  if (!/^https?:\/\//i.test(url)) {
    return "Base URL must start with http:// or https://";
  }
  if (/\s/.test(url)) {
    return "Base URL must not contain spaces.";
  }
  if (/^https?:\/\/[^/?#]*@/i.test(url)) {
    return "Base URL must not contain credentials (user:password@host). Put the API key in the API key field instead - base URLs are stored as plaintext metadata.";
  }
  if (model.trim() === "") {
    return "Model is required.";
  }

  const windowProblem = validateMaxContextTokens(maxContextTokens);
  if (windowProblem !== null) {
    return windowProblem;
  }

  const seen = new Set<string>();
  for (const [key] of pairs) {
    if (key === "") {
      return "Environment variable names must not be empty.";
    }
    if (key.includes("=") || key.includes("\0")) {
      return `Environment variable name "${key}" must not contain "=" or NUL.`;
    }
    if (seen.has(key)) {
      return `Environment variable "${key}" is defined twice.`;
    }
    seen.add(key);
  }

  return null;
}
