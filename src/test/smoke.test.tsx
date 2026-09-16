import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import EmptyState from "../components/EmptyState";

// Smoke test for the harness itself: EmptyState is a leaf component that only
// reads store state, so it renders with no Rust core and no mocking.
describe("EmptyState", () => {
  it("renders the no-workspace prompt and its primary action", () => {
    render(<EmptyState />);

    expect(
      screen.getByRole("heading", { name: /no workspace open/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /new workspace/i }),
    ).toBeInTheDocument();
  });
});
