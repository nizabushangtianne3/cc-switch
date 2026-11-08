import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Prompt } from "@/lib/api";
import PromptListItem from "@/components/prompts/PromptListItem";

const utf8BytesSpy = vi.fn();

// 计数透传：detail 现在由行自己算，基准要数「一条目算几次」。
vi.mock("@/components/prompts/promptUtils", async () => {
  const actual = await vi.importActual<
    typeof import("@/components/prompts/promptUtils")
  >("@/components/prompts/promptUtils");
  return {
    ...actual,
    utf8Bytes: (...args: Parameters<typeof actual.utf8Bytes>) =>
      utf8BytesSpy(...args) ?? actual.utf8Bytes(...args),
  };
});

const prompt = (overrides: Partial<Prompt> = {}): Prompt => ({
  id: overrides.id ?? "p1",
  name: overrides.name ?? "Alpha",
  content: overrides.content ?? "body",
  description: overrides.description,
  enabled: overrides.enabled ?? false,
  createdAt: overrides.createdAt ?? 1_700_000_000,
  updatedAt: overrides.updatedAt ?? 1_700_000_000,
});

const itemProps = {
  id: "p1",
  first: true,
  active: false,
  onToggle: vi.fn(),
  onEdit: vi.fn(),
  onDelete: vi.fn(),
};

describe("PromptListItem", () => {
  it("derives the second line from the description when there is one", () => {
    render(
      <PromptListItem
        {...itemProps}
        prompt={prompt({ description: "my desc", content: "x".repeat(2048) })}
      />,
    );
    const row = screen.getByTestId("prompt-row-p1");
    // 有说明就不读正文第一行；大小仍按整篇正文算
    expect(within(row).getByTitle(/my desc/)).toBeInTheDocument();
    expect(within(row).getByTitle(/2\.0 KB/)).toBeInTheDocument();
  });

  it("falls back to the first content line, then to the empty marker", () => {
    const { unmount } = render(
      <PromptListItem
        {...itemProps}
        prompt={prompt({ content: "# Heading\nrest" })}
      />,
    );
    expect(
      within(screen.getByTestId("prompt-row-p1")).getByTitle(/Heading/),
    ).toBeInTheDocument();
    unmount();

    render(<PromptListItem {...itemProps} prompt={prompt({ content: "" })} />);
    expect(
      within(screen.getByTestId("prompt-row-p1")).getByTitle(
        /prompts\.emptyContent/,
      ),
    ).toBeInTheDocument();
  });

  it("shows the enabled badge and disables delete for the enabled prompt", async () => {
    render(
      <PromptListItem
        {...itemProps}
        active
        prompt={prompt({ enabled: true, name: "Beta" })}
      />,
    );
    const row = screen.getByTestId("prompt-row-p1");
    expect(within(row).getByText("prompts.enabled")).toBeInTheDocument();
    expect(
      within(row).getByRole("button", { name: /prompts\.disableAria/ }),
    ).toBeInTheDocument();

    await userEvent.click(
      within(row).getByRole("button", { name: /prompts\.rowMoreActions/ }),
    );
    const menu = await screen.findByRole("menu");
    const deleteItem = within(menu)
      .getByText("common.delete")
      .closest("[role='menuitem']");
    // 启用中的提示词写进了目标文件，删库会让文件空掉：删除必须不可用
    expect(deleteItem).toHaveAttribute("data-disabled");
    expect(within(menu).getByText("prompts.deleteBlocked")).toBeInTheDocument();
  });

  it("keeps delete enabled for a disabled prompt", async () => {
    render(
      <PromptListItem {...itemProps} prompt={prompt({ enabled: false })} />,
    );
    await userEvent.click(
      within(screen.getByTestId("prompt-row-p1")).getByRole("button", {
        name: /prompts\.rowMoreActions/,
      }),
    );
    const menu = await screen.findByRole("menu");
    const deleteItem = within(menu)
      .getByText("common.delete")
      .closest("[role='menuitem']");
    expect(deleteItem).not.toHaveAttribute("data-disabled");
  });

  it("wires row actions to the handlers with the row id", async () => {
    const handlers = {
      onToggle: vi.fn(),
      onEdit: vi.fn(),
      onDelete: vi.fn(),
    };
    render(
      <PromptListItem
        {...itemProps}
        {...handlers}
        prompt={prompt({ enabled: false })}
      />,
    );
    const row = screen.getByTestId("prompt-row-p1");
    await userEvent.click(
      within(row).getByRole("button", { name: /prompts\.enableAria/ }),
    );
    expect(handlers.onToggle).toHaveBeenCalledWith("p1", true);
    await userEvent.click(
      within(row).getByRole("button", { name: /prompts\.editAria/ }),
    );
    expect(handlers.onEdit).toHaveBeenCalledWith("p1");
  });

  it("opens the editor from a row click but not from a button click", async () => {
    const onEdit = vi.fn();
    const onToggle = vi.fn();
    render(
      <PromptListItem
        {...itemProps}
        onEdit={onEdit}
        onToggle={onToggle}
        prompt={prompt()}
      />,
    );
    const row = screen.getByTestId("prompt-row-p1");
    await userEvent.click(within(row).getByText("Alpha"));
    expect(onEdit).toHaveBeenCalledWith("p1");

    onEdit.mockClear();
    await userEvent.click(
      within(row).getByRole("button", { name: /prompts\.enableAria/ }),
    );
    // 按钮落在行容器的 button 上：行点击的 closest 守卫必须拦住重复打开
    expect(onToggle).toHaveBeenCalledWith("p1", true);
    expect(onEdit).not.toHaveBeenCalled();
  });

  it("ignores a row click while the row is disabled", async () => {
    const onEdit = vi.fn();
    render(
      <PromptListItem
        {...itemProps}
        disabled
        onEdit={onEdit}
        prompt={prompt()}
      />,
    );
    const row = screen.getByTestId("prompt-row-p1");
    await userEvent.click(within(row).getByText("Alpha"));
    expect(onEdit).not.toHaveBeenCalled();
  });

  // memo 基准：同一个 element 复用父组件重渲染时不得牵连；但 rerender 必须
  // 每次构造新 element，否则 React 对整棵 bailout、基准失效。这里 rerender
  // 新 element 且 props 完全相同：行必须整体 bailout，detail 不得重算。
  it("computes the detail line once and never recomputes it on an identical re-render (row memo baseline)", () => {
    // 同一引用：新建对象会让 memo 浅比较失败，基准就测不到 bailout。
    const stablePrompt = prompt({ content: "x".repeat(512) });
    const { rerender } = render(
      <PromptListItem {...itemProps} prompt={stablePrompt} />,
    );
    expect(utf8BytesSpy.mock.calls.length).toBe(1);

    rerender(<PromptListItem {...itemProps} prompt={stablePrompt} />);
    expect(utf8BytesSpy.mock.calls.length).toBe(1);
  });
});
