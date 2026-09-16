/**
 * Provider management UI (spec sections 5, 12, 17).
 *
 * Covers the panel (list, badges, delete confirmation, backend errors) and the
 * form (client-side validation, extra environment variables, secret handling).
 * API keys only ever cross the bridge through `set_provider_secret`; the tests
 * assert that a profile payload never carries one.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import ProvidersPanel from "../components/ProvidersPanel";
import ProviderForm from "../components/ProviderForm";
import type { ProviderInput, ProviderProfile } from "../types";
import { makeProvider, resetAppStore, seedProviders, setupUser } from "./fixtures";
import { invokeMock, resetTauriMock, stubCommands, tauriRuntime } from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);

const PROVIDER_A = makeProvider({
  extraEnv: [["ANTHROPIC_AUTH_TOKEN", "gateway-secret-value"]],
});
const PROVIDER_B = makeProvider({
  id: "prov-2",
  name: "Provider B",
  baseUrl: "https://provider-b.example.com",
  model: "model-b",
});

/** The currently open editor form. */
function editorForm(): HTMLElement {
  return screen.getByRole("heading", { name: /new provider|edit/i }).closest("form") as HTMLElement;
}

/** The panel offers the create action in its header and in the empty state. */
function openCreateForm(user: ReturnType<typeof setupUser>): Promise<void> {
  return user.click(screen.getAllByRole("button", { name: "+ Add provider" })[0]);
}

async function fillProviderForm(
  user: ReturnType<typeof setupUser>,
  values: {
    name: string;
    baseUrl: string;
    model: string;
    /** Omitted leaves the optional context-window field untouched (empty). */
    maxContextTokens?: string;
    apiKey?: string;
  },
): Promise<void> {
  const form = editorForm();
  await user.type(within(form).getByLabelText(/^name/i), values.name);
  await user.type(within(form).getByLabelText(/^base url/i), values.baseUrl);
  await user.type(within(form).getByLabelText(/^model/i), values.model);
  if (values.maxContextTokens !== undefined) {
    await user.type(
      within(form).getByLabelText(/^max context tokens/i),
      values.maxContextTokens,
    );
  }
  if (values.apiKey !== undefined) {
    await user.type(within(form).getByLabelText(/^api key/i), values.apiKey);
  }
}

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

describe("ProvidersPanel: the list", () => {
  it("renders every profile with its endpoint, model and keyring badge", () => {
    seedProviders([PROVIDER_A, PROVIDER_B], {
      "prov-1": true,
      "prov-2": false,
    });

    render(<ProvidersPanel />);

    expect(screen.getByText("2 configured")).toBeInTheDocument();

    const cards = screen.getAllByRole("listitem");
    expect(cards).toHaveLength(2);

    expect(within(cards[0]).getByText("Provider A")).toBeInTheDocument();
    expect(
      within(cards[0]).getByText("https://provider-a.example.com"),
    ).toBeInTheDocument();
    expect(within(cards[0]).getByText("model-a")).toBeInTheDocument();
    expect(within(cards[0]).getByText("key set")).toBeInTheDocument();

    expect(within(cards[1]).getByText("no key")).toBeInTheDocument();
  });

  it("says so when the keyring could not be read, instead of guessing", () => {
    seedProviders([PROVIDER_A], { "prov-1": null });

    render(<ProvidersPanel />);

    expect(screen.getByText("key status unknown")).toBeInTheDocument();
  });

  it("lists extra environment variable names but never their values", () => {
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);

    expect(screen.getByText("ANTHROPIC_AUTH_TOKEN")).toBeInTheDocument();
    // A gateway token may live in that value (spec section 17).
    expect(screen.queryByText("gateway-secret-value")).not.toBeInTheDocument();
  });

  it("degrades to a notice when the Rust core is not there", async () => {
    tauriRuntime.available = false;

    render(<ProvidersPanel />);

    expect(screen.getByLabelText("Backend unavailable")).toHaveTextContent(
      /tauri dev/,
    );
    expect(screen.getByRole("button", { name: "+ Add provider" })).toBeDisabled();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("ProviderForm: client-side validation", () => {
  it("rejects a base URL without a scheme, before any backend call", async () => {
    const user = setupUser();
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "provider-c.example.com",
      model: "model-c",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    expect(within(editorForm()).getByRole("alert")).toHaveTextContent(
      "Base URL must start with http:// or https://",
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("rejects credentials embedded in the base URL", async () => {
    const user = setupUser();
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "https://user:password@provider-c.example.com",
      model: "model-c",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    expect(within(editorForm()).getByRole("alert")).toHaveTextContent(
      /must not contain credentials/i,
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("shapes extra environment variables as pairs and drops blank rows", async () => {
    const user = setupUser();
    const onSubmit = vi.fn(async (_input: ProviderInput, _apiKey: string) => true);

    render(
      <ProviderForm
        secretStatus={null}
        submitting={false}
        onCancel={vi.fn()}
        onSubmit={onSubmit}
        onClearSecret={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText(/^name/i), "Provider C");
    await user.type(screen.getByLabelText(/^base url/i), "https://provider-c.example.com");
    await user.type(screen.getByLabelText(/^model/i), "model-c");

    // One row left blank, one row filled: only the filled one is sent.
    await user.click(screen.getByRole("button", { name: "+ Add variable" }));
    await user.click(screen.getByRole("button", { name: "+ Add variable" }));
    await user.type(
      screen.getByLabelText("Environment variable 1 name"),
      "ANTHROPIC_AUTH_TOKEN",
    );
    await user.type(
      screen.getByLabelText("Environment variable 1 value"),
      "gateway-token",
    );

    await user.click(screen.getByRole("button", { name: "Create provider" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        {
          name: "Provider C",
          baseUrl: "https://provider-c.example.com",
          model: "model-c",
          extraEnv: [["ANTHROPIC_AUTH_TOKEN", "gateway-token"]],
          // An untouched window field is "not declared", which the backend
          // must receive as `null` - not as `0`.
          maxContextTokens: null,
        },
        "",
      ),
    );
  });

  it("rejects a declared context window below 1000, before any backend call", async () => {
    const user = setupUser();
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "https://provider-c.example.com",
      model: "model-c",
      maxContextTokens: "500",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    expect(within(editorForm()).getByRole("alert")).toHaveTextContent(
      /max context tokens must be at least 1000/i,
    );
    // The rule mirrors the backend, so nothing is sent to be rejected there.
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("rejects a duplicated environment variable name", async () => {
    const user = setupUser();
    const onSubmit = vi.fn(async (_input: ProviderInput, _apiKey: string) => true);

    render(
      <ProviderForm
        secretStatus={null}
        submitting={false}
        onCancel={vi.fn()}
        onSubmit={onSubmit}
        onClearSecret={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText(/^name/i), "Provider C");
    await user.type(screen.getByLabelText(/^base url/i), "https://provider-c.example.com");
    await user.type(screen.getByLabelText(/^model/i), "model-c");
    await user.click(screen.getByRole("button", { name: "+ Add variable" }));
    await user.click(screen.getByRole("button", { name: "+ Add variable" }));
    await user.type(
      screen.getByLabelText("Environment variable 1 name"),
      "ANTHROPIC_AUTH_TOKEN",
    );
    await user.type(
      screen.getByLabelText("Environment variable 2 name"),
      "ANTHROPIC_AUTH_TOKEN",
    );

    await user.click(screen.getByRole("button", { name: "Create provider" }));

    expect(screen.getByRole("alert")).toHaveTextContent(/defined twice/i);
    expect(onSubmit).not.toHaveBeenCalled();
  });
});

describe("ProvidersPanel: creating a provider", () => {
  it("sends the profile without the key, then stores the key in the keyring", async () => {
    const user = setupUser();
    let rows: ProviderProfile[] = [PROVIDER_A];
    stubCommands({
      create_provider: (args) => {
        const input = args?.input as ProviderInput;
        const created: ProviderProfile = { id: "prov-9", ...input };
        rows = [...rows, created];
        return created;
      },
      list_providers: () => rows,
      provider_secret_status: () => true,
      set_provider_secret: () => null,
    });
    seedProviders(rows, { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "https://provider-c.example.com",
      model: "model-c",
      apiKey: "sk-test-key",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("create_provider", {
        input: {
          name: "Provider C",
          baseUrl: "https://provider-c.example.com",
          model: "model-c",
          extraEnv: [],
          maxContextTokens: null,
        },
      }),
    );
    // The key never appears in the profile payload (spec sections 5, 17).
    const createCall = invokeMock.mock.calls.find(
      ([command]) => command === "create_provider",
    );
    expect(JSON.stringify(createCall?.[1])).not.toContain("sk-test-key");
    expect(invokeMock).toHaveBeenCalledWith("set_provider_secret", {
      providerId: "prov-9",
      apiKey: "sk-test-key",
    });

    // The form closes and the new profile shows up with its stored key.
    await waitFor(() =>
      expect(screen.queryByRole("heading", { name: "New provider" })).toBeNull(),
    );
    expect(await screen.findByText("Provider C")).toBeInTheDocument();
  });

  it("keeps the form open and shows why when the backend rejects the profile", async () => {
    const user = setupUser();
    stubCommands({
      create_provider: () => {
        throw 'provider "Provider C" already exists';
      },
      list_providers: () => [PROVIDER_A],
      provider_secret_status: () => true,
    });
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "https://provider-c.example.com",
      model: "model-c",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      'provider "Provider C" already exists',
    );
    // Nothing was lost, so the user can fix the name and try again.
    expect(within(editorForm()).getByLabelText(/^name/i)).toHaveValue(
      "Provider C",
    );
  });
});

describe("ProvidersPanel: editing and secrets", () => {
  it("keeps the stored key when the key field is left blank", async () => {
    const user = setupUser();
    stubCommands({
      update_provider: () => PROVIDER_A,
      list_providers: () => [PROVIDER_A],
      provider_secret_status: () => true,
      set_provider_secret: () => null,
    });
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await user.click(screen.getByRole("button", { name: "Edit" }));

    expect(
      within(editorForm()).getByText(/leave this blank to keep it/i),
    ).toBeInTheDocument();
    await user.click(
      within(editorForm()).getByRole("button", { name: "Save changes" }),
    );

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("update_provider", {
        id: "prov-1",
        input: {
          name: "Provider A",
          baseUrl: "https://provider-a.example.com",
          model: "model-a",
          extraEnv: [["ANTHROPIC_AUTH_TOKEN", "gateway-secret-value"]],
          maxContextTokens: null,
        },
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "set_provider_secret",
      expect.anything(),
    );
  });

  it("removing the stored key clears the keyring entry", async () => {
    const user = setupUser();
    stubCommands({
      set_provider_secret: () => null,
      list_providers: () => [PROVIDER_A],
      provider_secret_status: () => false,
    });
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await user.click(screen.getByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("button", { name: "Remove stored key" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_provider_secret", {
        providerId: "prov-1",
        apiKey: "",
      }),
    );
  });
});

describe("ProvidersPanel: the declared context window", () => {
  it("lists the declared window as a chip, and nothing when it is undeclared", () => {
    seedProviders(
      [
        makeProvider({ id: "prov-1", name: "Provider A", maxContextTokens: 200_000 }),
        makeProvider({
          id: "prov-2",
          name: "Provider B",
          model: "glm-5.3",
          maxContextTokens: 1_000_000,
        }),
        makeProvider({ id: "prov-3", name: "Provider C", model: "model-c" }),
      ],
      { "prov-1": true, "prov-2": true, "prov-3": true },
    );

    render(<ProvidersPanel />);

    const cards = screen.getAllByRole("listitem");
    expect(
      within(cards[0]).getByTitle(/declared context window/i),
    ).toHaveTextContent("200k ctx");
    expect(
      within(cards[1]).getByTitle(/declared context window/i),
    ).toHaveTextContent("1M ctx");
    // No declared window: no chip to explain, because nothing is overridden.
    expect(within(cards[2]).queryByTitle(/declared context window/i)).toBeNull();
  });

  it("sends the declared window on create and keeps it in the list", async () => {
    const user = setupUser();
    let rows: ProviderProfile[] = [PROVIDER_A];
    stubCommands({
      create_provider: (args) => {
        const input = args?.input as ProviderInput;
        const created: ProviderProfile = { id: "prov-9", ...input };
        rows = [...rows, created];
        return created;
      },
      list_providers: () => rows,
      provider_secret_status: () => true,
    });
    seedProviders(rows, { "prov-1": true });

    render(<ProvidersPanel />);
    await openCreateForm(user);
    await fillProviderForm(user, {
      name: "Provider C",
      baseUrl: "https://provider-c.example.com",
      model: "glm-5.3",
      maxContextTokens: "200000",
    });
    await user.click(screen.getByRole("button", { name: "Create provider" }));

    // The declared window reaches the backend as a number, not as a string.
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("create_provider", {
        input: {
          name: "Provider C",
          baseUrl: "https://provider-c.example.com",
          model: "glm-5.3",
          extraEnv: [],
          maxContextTokens: 200_000,
        },
      }),
    );

    const card = (await screen.findByText("Provider C")).closest(
      "li",
    ) as HTMLElement;
    expect(within(card).getByTitle(/declared context window/i)).toHaveTextContent(
      "200k ctx",
    );
  });

  it("round-trips the window through update, and an emptied field sends null", async () => {
    const user = setupUser();
    const declared = makeProvider({ maxContextTokens: 200_000 });
    const inputs: ProviderInput[] = [];
    let rows: ProviderProfile[] = [declared];
    stubCommands({
      update_provider: (args) => {
        const input = args?.input as ProviderInput;
        inputs.push(input);
        const updated: ProviderProfile = {
          ...declared,
          ...input,
          maxContextTokens: input.maxContextTokens ?? null,
        };
        rows = [updated];
        return updated;
      },
      list_providers: () => rows,
      provider_secret_status: () => true,
    });
    seedProviders(rows, { "prov-1": true });

    render(<ProvidersPanel />);
    await user.click(screen.getByRole("button", { name: "Edit" }));

    // The stored window is loaded into the field, so an edit starts from it.
    const field = within(editorForm()).getByLabelText(/^max context tokens/i);
    expect(field).toHaveValue(200_000);

    await user.clear(field);
    await user.type(field, "1000000");
    await user.click(
      within(editorForm()).getByRole("button", { name: "Save changes" }),
    );

    await waitFor(() => expect(inputs).toHaveLength(1));
    expect(inputs[0].maxContextTokens).toBe(1_000_000);
    // A saved window is visible without reopening the form.
    await waitFor(() =>
      expect(screen.getByTitle(/declared context window/i)).toHaveTextContent(
        "1M ctx",
      ),
    );

    // Emptying the field means "not declared" - `null`, never `0`, which the
    // backend rejects because no real model has a zero-token window.
    await user.click(screen.getByRole("button", { name: "Edit" }));
    const reopened = within(editorForm()).getByLabelText(/^max context tokens/i);
    expect(reopened).toHaveValue(1_000_000);
    await user.clear(reopened);
    await user.click(
      within(editorForm()).getByRole("button", { name: "Save changes" }),
    );

    await waitFor(() => expect(inputs).toHaveLength(2));
    expect(inputs[1].maxContextTokens).toBeNull();
  });
});

describe("ProvidersPanel: delete confirmation", () => {
  it("deletes nothing until the confirmation is accepted", async () => {
    const user = setupUser();
    let rows: ProviderProfile[] = [PROVIDER_A, PROVIDER_B];
    stubCommands({
      delete_provider: () => {
        rows = [PROVIDER_B];
        return null;
      },
      list_providers: () => rows,
      provider_secret_status: () => false,
    });
    seedProviders(rows, { "prov-1": true, "prov-2": false });

    render(<ProvidersPanel />);
    await user.click(screen.getAllByRole("button", { name: "Delete" })[0]);

    expect(invokeMock).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Confirm delete" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(
      /also removes its stored API key/i,
    );

    // Backing out leaves everything alone.
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("button", { name: "Confirm delete" })).toBeNull();
    expect(screen.getByText("Provider A")).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();

    await user.click(screen.getAllByRole("button", { name: "Delete" })[0]);
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("delete_provider", {
        id: "prov-1",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByText("Provider A")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Provider B")).toBeInTheDocument();
  });
});

describe("ProvidersPanel: connectivity test", () => {
  it("shows the result inline and lets the user dismiss it", async () => {
    const user = setupUser();
    stubCommands({
      test_provider: () => ({
        ok: false,
        status: 401,
        message: "unauthorized",
      }),
    });
    seedProviders([PROVIDER_A], { "prov-1": true });

    render(<ProvidersPanel />);
    await user.click(screen.getByRole("button", { name: "Test" }));

    const result = await screen.findByRole("status");
    expect(result).toHaveTextContent("FAILED · HTTP 401");
    expect(result).toHaveTextContent("unauthorized");

    await user.click(screen.getByRole("button", { name: "Dismiss" }));

    expect(screen.queryByRole("status")).toBeNull();
  });
});
