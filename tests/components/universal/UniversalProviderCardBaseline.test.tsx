import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { http, HttpResponse } from "msw";
import type { UniversalProvider } from "@/types";
import { UniversalProviderPanel } from "@/components/universal/UniversalProviderPanel";
import { server } from "../../msw/server";

const TAURI_ENDPOINT = "http://tauri.local";

// 卡片渲染探针：每张卡每次渲染都会调一次 ProviderIcon。
const iconSpy = vi.hoisted(() => vi.fn());

vi.mock("@/components/ProviderIcon", () => ({
  ProviderIcon: (props: { name: string }) => {
    iconSpy(props.name);
    return <span data-testid={`icon-${props.name}`} />;
  },
}));

// 表单模态框很重，基准只关心列表：换成轻量替身，但保留 isOpen 语义。
vi.mock("@/components/universal/UniversalProviderFormModal", () => ({
  UniversalProviderFormModal: ({ isOpen }: { isOpen: boolean }) =>
    isOpen ? <div data-testid="universal-form" /> : null,
}));

const provider = (id: string, name: string): UniversalProvider => ({
  id,
  name,
  providerType: "newapi",
  apps: { claude: true, codex: false, gemini: false },
  baseUrl: "https://example.com",
  apiKey: "k",
  models: {},
});

const renderPanel = () => {
  server.use(
    http.post(`${TAURI_ENDPOINT}/get_universal_providers`, () =>
      HttpResponse.json({
        "p-1": provider("p-1", "Alpha"),
        "p-2": provider("p-2", "Beta"),
        "p-3": provider("p-3", "Gamma"),
      }),
    ),
  );
  return render(<UniversalProviderPanel />);
};

const rendersOf = (name: string) =>
  iconSpy.mock.calls.filter((call) => call[0] === name).length;

describe("universal provider card render baseline", () => {
  it("does not re-render the other cards when one card opens the editor", async () => {
    renderPanel();
    await waitFor(() =>
      expect(screen.getByTestId("icon-Alpha")).toBeInTheDocument(),
    );
    const before = {
      alpha: rendersOf("Alpha"),
      beta: rendersOf("Beta"),
      gamma: rendersOf("Gamma"),
    };
    // 前提自断言：3 张卡都渲染了
    expect(Object.values(before).every((count) => count > 0)).toBe(true);

    // 打开第一张卡的编辑器：面板状态变了，但另外两张卡的 props 一点没动
    const user = userEvent.setup();
    const alphaCard = screen.getByTestId("icon-Alpha").closest("div.group")!;
    // 测试 i18n 没有资源，t() 回落 defaultValue：按钮名是中文
    await user.click(
      within(alphaCard as HTMLElement).getByRole("button", {
        name: "编辑",
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId("universal-form")).toBeInTheDocument(),
    );

    expect(rendersOf("Beta")).toBe(before.beta);
    expect(rendersOf("Gamma")).toBe(before.gamma);
  });
});
