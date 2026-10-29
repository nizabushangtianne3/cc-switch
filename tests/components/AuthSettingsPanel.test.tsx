import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AuthSettingsPanel } from "@/components/providers/AuthSettingsPanel";

vi.mock("@/components/settings/AuthCenterPanel", () => ({
  AuthCenterPanel: () => <div data-testid="auth-center-loaded">loaded</div>,
}));

describe("AuthSettingsPanel", () => {
  it("loads the auth center only after a target is opened", async () => {
    const { rerender } = render(
      <AuthSettingsPanel target={null} onClose={vi.fn()} />,
    );

    expect(screen.queryByTestId("auth-center-loaded")).not.toBeInTheDocument();

    rerender(<AuthSettingsPanel target="github_copilot" onClose={vi.fn()} />);

    expect(await screen.findByTestId("auth-center-loaded")).toBeInTheDocument();
  });
});
