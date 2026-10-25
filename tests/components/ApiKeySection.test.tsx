import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiKeySection } from "@/components/providers/forms/shared/ApiKeySection";

// 测试的 i18n 是空资源，推广语传了 defaultValue: "" 会变成空串；这里让 t() 原样返回 key
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

function renderSection(
  props: Partial<React.ComponentProps<typeof ApiKeySection>> = {},
) {
  return render(
    <ApiKeySection
      value=""
      onChange={() => {}}
      category="third_party"
      shouldShowLink
      websiteUrl="https://example.com/keys"
      partnerPromotionKey="kimi"
      {...props}
    />,
  );
}

describe("ApiKeySection", () => {
  it("shows the preset's promotion under the input next to the API Key link", () => {
    renderSection();

    expect(
      screen.getByRole("link", { name: /providerForm\.getApiKey/ }),
    ).toHaveAttribute("href", "https://example.com/keys");
    expect(
      screen.getByText("providerForm.partnerPromotion.kimi"),
    ).toBeInTheDocument();
  });

  it("hides the promotion when the API Key link is hidden", () => {
    renderSection({ category: "official", shouldShowLink: false });

    expect(
      screen.queryByText("providerForm.partnerPromotion.kimi"),
    ).not.toBeInTheDocument();
  });

  it("renders nothing extra for presets without a promotion", () => {
    const { container } = renderSection({ partnerPromotionKey: undefined });

    expect(container.querySelector("p")).toBeNull();
  });

  it("renders one input per key and shows weight fields only in weighted mode", () => {
    const { container, rerender } = render(
      <ApiKeySection
        value=""
        onChange={() => {}}
        category="third_party"
        shouldShowLink={false}
        websiteUrl=""
        apiKeys={[
          { key: "sk-a", weight: 1 },
          { key: "sk-b", weight: 2 },
        ]}
        apiKeyStrategy="round_robin"
        onApiKeysChange={() => {}}
        onApiKeyStrategyChange={() => {}}
      />,
    );

    expect(container.querySelectorAll('input[type="password"]')).toHaveLength(
      2,
    );
    expect(container.querySelectorAll('input[type="number"]')).toHaveLength(0);

    rerender(
      <ApiKeySection
        value=""
        onChange={() => {}}
        category="third_party"
        shouldShowLink={false}
        websiteUrl=""
        apiKeys={[
          { key: "sk-a", weight: 1 },
          { key: "sk-b", weight: 2 },
        ]}
        apiKeyStrategy="weighted"
        onApiKeysChange={() => {}}
        onApiKeyStrategyChange={() => {}}
      />,
    );

    expect(container.querySelectorAll('input[type="number"]')).toHaveLength(2);
  });

  it("formats a pasted list into separate key rows", () => {
    const onChange = vi.fn();
    const { container } = render(
      <ApiKeySection
        value=""
        onChange={() => {}}
        category="third_party"
        shouldShowLink={false}
        websiteUrl=""
        apiKeys={[{ key: "", weight: 1 }]}
        apiKeyStrategy="round_robin"
        onApiKeysChange={onChange}
        onApiKeyStrategyChange={() => {}}
      />,
    );

    fireEvent.paste(container.querySelector('input[type="password"]')!, {
      clipboardData: { getData: () => "sk-a\nsk-b\nsk-c" },
    });

    expect(onChange).toHaveBeenCalledWith([
      { key: "sk-a", weight: 1 },
      { key: "sk-b", weight: 1 },
      { key: "sk-c", weight: 1 },
    ]);
  });

  it("turns off autocorrect and auto-capitalization on the key input", () => {
    const { container } = renderSection({ value: "sk-test" });
    const input = container.querySelector("input");

    expect(input).toHaveAttribute("autocorrect", "off");
    expect(input).toHaveAttribute("autocapitalize", "off");
    expect(input).toHaveAttribute("spellcheck", "false");
  });
});
