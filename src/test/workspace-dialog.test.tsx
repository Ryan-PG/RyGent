/**
 * Workspace creation dialog (spec section 12, "New Workspace").
 *
 * The form is rendered with explicit props, so validation and payload shaping
 * are asserted without a store; the last test drives the whole shell (`App`) to
 * prove the submit really reaches `create_workspace`.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  render,
  screen,
  waitFor,
  within,
  type RenderResult,
} from "@testing-library/react";
import App from "../App";
import WorkspaceDialog from "../components/WorkspaceDialog";
import { useAppStore } from "../stores/useAppStore";
import type { Workspace, WorkspaceInput } from "../types";
import { makeProvider, makeWorkspace, resetAppStore, setupUser } from "./fixtures";
import {
  dialogOpenMock,
  invokeMock,
  resetTauriMock,
  stubCommands,
  tauriRuntime,
} from "./mock-tauri";

vi.mock("@tauri-apps/api/core", async () =>
  (await import("./mock-tauri")).tauriCoreModule(),
);
vi.mock("@tauri-apps/plugin-dialog", async () =>
  (await import("./mock-tauri")).tauriDialogModule(),
);

const PROVIDERS = [
  makeProvider(),
  makeProvider({ id: "prov-2", name: "Provider B", model: "model-b" }),
];

type DialogProps = Parameters<typeof WorkspaceDialog>[0];

/** Render the dialog with working spies, applying any per-test overrides. */
function renderDialog(
  overrides: Partial<DialogProps> = {},
): RenderResult & {
  onSubmit: ReturnType<typeof vi.fn>;
  onCancel: ReturnType<typeof vi.fn>;
  onOpenProviders: ReturnType<typeof vi.fn>;
} {
  const onSubmit = vi.fn(async (_input: WorkspaceInput) => true);
  const onCancel = vi.fn();
  const onOpenProviders = vi.fn();
  const view = render(
    <WorkspaceDialog
      providers={PROVIDERS}
      submitting={false}
      error={null}
      {...{ onSubmit, onCancel, onOpenProviders }}
      {...overrides}
    />,
  );
  return { ...view, onSubmit, onCancel, onOpenProviders };
}

const nameField = () => screen.getByLabelText(/^name$/i);
const pathField = () => screen.getByLabelText(/^project folder/i);
const modelField = () => screen.getByLabelText(/^model/i);
const providerSelect = () =>
  screen.getByRole("combobox", { name: /^provider/i });
const submitButton = (label: string | RegExp = /create workspace|save changes/i) =>
  screen.getByRole("button", { name: label });

beforeEach(() => {
  resetAppStore();
  resetTauriMock();
});

describe("WorkspaceDialog: validation", () => {
  it("requires a name first, then a project folder", async () => {
    const user = setupUser();
    const { onSubmit } = renderDialog();

    await user.click(submitButton());
    expect(screen.getByRole("alert")).toHaveTextContent("Name is required.");
    expect(onSubmit).not.toHaveBeenCalled();

    await user.type(nameField(), "Alpha");
    await user.click(submitButton());
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Project folder is required.",
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("submits trimmed values, the supported agent and the selected provider's model", async () => {
    const user = setupUser();
    const { onSubmit } = renderDialog();

    await user.type(nameField(), "  Alpha  ");
    await user.type(pathField(), "  D:\\Projects\\alpha  ");
    await user.click(submitButton());

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith({
        name: "Alpha",
        projectPath: "D:\\Projects\\alpha",
        agentId: "claude-code",
        providerId: "prov-1",
        model: "model-a",
      }),
    );
  });

  it("refuses to create anything while no provider profile exists", async () => {
    const user = setupUser();
    const { onSubmit, onOpenProviders } = renderDialog({ providers: [] });

    expect(submitButton()).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent(
      /no provider profiles yet/i,
    );

    await user.click(screen.getByRole("button", { name: "Go to Providers" }));

    expect(onOpenProviders).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });
});

describe("WorkspaceDialog: provider selection drives the model", () => {
  it("moves the model to the new provider's default and keeps it editable", async () => {
    const user = setupUser();
    const { onSubmit } = renderDialog();

    expect(modelField()).toHaveValue("model-a");

    await user.selectOptions(providerSelect(), "prov-2");
    expect(modelField()).toHaveValue("model-b");

    await user.clear(modelField());
    await user.type(modelField(), "model-custom");
    expect(modelField()).toHaveValue("model-custom");

    await user.type(nameField(), "Alpha");
    await user.type(pathField(), "D:\\Projects\\alpha");
    await user.click(submitButton());

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          providerId: "prov-2",
          model: "model-custom",
        }),
      ),
    );
  });

  it("sends null for a cleared model, meaning 'use the provider default'", async () => {
    const user = setupUser();
    const { onSubmit } = renderDialog({
      workspace: makeWorkspace({ model: "model-a" }),
    });

    expect(modelField()).toHaveValue("model-a");
    await user.clear(modelField());
    await user.click(submitButton(/save changes/i));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({ model: null }),
      ),
    );
  });
});

describe("WorkspaceDialog: edit mode", () => {
  it("prefills the workspace being edited, including its model override", () => {
    renderDialog({
      workspace: makeWorkspace({ name: "Beta", model: "model-override" }),
    });

    expect(
      screen.getByRole("dialog", { name: "Edit workspace" }),
    ).toBeInTheDocument();
    expect(nameField()).toHaveValue("Beta");
    expect(pathField()).toHaveValue("D:\\Projects\\alpha");
    expect(modelField()).toHaveValue("model-override");
    expect(submitButton(/save changes/i)).toBeInTheDocument();
  });
});

describe("WorkspaceDialog: backend errors", () => {
  it("renders a failure as a message and leaves the form usable", () => {
    renderDialog({ error: "project folder does not exist: D:\\nope" });

    expect(screen.getByRole("alert")).toHaveTextContent(
      "project folder does not exist: D:\\nope",
    );
    expect(nameField()).toBeEnabled();
    expect(pathField()).toBeEnabled();
    expect(submitButton()).toBeEnabled();
  });

  it("shows a save that is in flight and disables the submit button", () => {
    renderDialog({ submitting: true });
    expect(submitButton(/saving/i)).toBeDisabled();
  });
});

describe("WorkspaceDialog: project folder picker", () => {
  it("explains why the native picker is missing and keeps the path editable", async () => {
    tauriRuntime.available = false;
    const user = setupUser();
    renderDialog();

    await user.click(screen.getByRole("button", { name: "Browse…" }));

    expect(
      screen.getByText(/desktop folder picker needs the Rust core/i),
    ).toBeInTheDocument();

    await user.type(pathField(), "D:\\typed\\by-hand");
    expect(pathField()).toHaveValue("D:\\typed\\by-hand");
  });

  it("fills the folder field from the native picker when it is available", async () => {
    dialogOpenMock.mockResolvedValueOnce("D:\\Picked\\folder");
    const user = setupUser();
    renderDialog();

    await user.click(screen.getByRole("button", { name: "Browse…" }));

    await waitFor(() => expect(pathField()).toHaveValue("D:\\Picked\\folder"));
  });
});

describe("WorkspaceDialog: end-to-end through the shell", () => {
  it("creating a workspace from the empty state opens a tab for it", async () => {
    const user = setupUser();
    // A one-row database: create appends, list returns what exists.
    let rows: Workspace[] = [];
    stubCommands({
      list_workspaces: () => rows,
      load_workspace_layout: () => ({
        openWorkspaceIds: [],
        activeWorkspaceId: null,
      }),
      save_workspace_layout: () => null,
      list_providers: () => PROVIDERS,
      provider_secret_status: () => false,
      create_workspace: (args) => {
        const input = args?.input as WorkspaceInput;
        const created = makeWorkspace({
          id: "ws-new",
          name: input.name,
          projectPath: input.projectPath,
          providerId: input.providerId,
          model: input.model,
        });
        rows = [created];
        return created;
      },
    });

    render(<App />);

    const main = await screen.findByRole("main");
    await user.click(
      within(main).getByRole("button", { name: /new workspace/i }),
    );

    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    await user.type(within(dialog).getByLabelText(/^name$/i), "Delta");
    await user.type(
      within(dialog).getByLabelText(/^project folder/i),
      "D:\\Projects\\delta",
    );
    await user.click(
      within(dialog).getByRole("button", { name: "Create workspace" }),
    );

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("create_workspace", {
        input: {
          name: "Delta",
          projectPath: "D:\\Projects\\delta",
          agentId: "claude-code",
          providerId: "prov-1",
          model: "model-a",
        },
      }),
    );

    // The dialog closes and the new workspace is open in a tab (spec 9, 10).
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await screen.findByRole("tab", { name: /Delta/ })).toBeInTheDocument();
    expect(useAppStore.getState()).toMatchObject({
      activeTabId: "ws-new",
      workspacesError: null,
    });
  });
});
