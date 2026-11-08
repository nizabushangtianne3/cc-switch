import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Prompt } from "@/lib/api";
import {
  PromptLibrary,
  filterPromptEntries,
} from "@/components/prompts/PromptLibrary";

const promptRowSpy = vi.fn();
const firstLineSpy = vi.fn();
const utf8BytesSpy = vi.fn();
const formatSizeSpy = vi.fn();
const formatUpdatedSpy = vi.fn();

// 计数透传：基准要数「一次按键重算了多少行 detail」，实现仍走真实函数。
vi.mock("@/components/prompts/promptUtils", async () => {
  const actual = await vi.importActual<
    typeof import("@/components/prompts/promptUtils")
  >("@/components/prompts/promptUtils");
  return {
    ...actual,
    firstLine: (...args: Parameters<typeof actual.firstLine>) =>
      firstLineSpy(...args) ?? actual.firstLine(...args),
    utf8Bytes: (...args: Parameters<typeof actual.utf8Bytes>) =>
      utf8BytesSpy(...args) ?? actual.utf8Bytes(...args),
    formatSize: (...args: Parameters<typeof actual.formatSize>) =>
      formatSizeSpy(...args) ?? actual.formatSize(...args),
    formatUpdated: (...args: Parameters<typeof actual.formatUpdated>) =>
      formatUpdatedSpy(...args) ?? actual.formatUpdated(...args),
  };
});

vi.mock("@/components/prompts/PromptListItem", async () => {
  const { memo } = await vi.importActual<typeof import("react")>("react");
  return {
    // mock 也要包 memo：否则父组件重渲染时必然调用它，基准测不到 memo 契约。
    default: memo((props: any) => {
      promptRowSpy(props);
      return (
        <li data-testid={`prompt-row-${props.id}`}>
          <span>{props.prompt.name}</span>
          <button
            type="button"
            onClick={() => props.onToggle(props.id, !props.active)}
          >
            toggle
          </button>
          <button type="button" onClick={() => props.onEdit(props.id)}>
            edit
          </button>
          <button type="button" onClick={() => props.onDelete(props.id)}>
            delete
          </button>
          {props.onCopyToApps ? (
            <button type="button" onClick={() => props.onCopyToApps(props.id)}>
              copy-apps
            </button>
          ) : null}
          {props.onCopyContent ? (
            <button type="button" onClick={() => props.onCopyContent(props.id)}>
              copy-content
            </button>
          ) : null}
        </li>
      );
    }),
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

const libraryProps = {
  listLabel: "prompts",
  onClearSearch: () => {},
  onToggle: () => {},
  onEdit: () => {},
  onDelete: () => {},
};

describe("filterPromptEntries", () => {
  const entries: Array<[string, Prompt]> = [
    ["p1", prompt({ id: "p1", name: "Alpha" })],
    [
      "p2",
      prompt({ id: "p2", name: "Beta", content: "contains needle inside" }),
    ],
  ];

  it("returns everything for a blank query", () => {
    expect(filterPromptEntries(entries, "   ")).toHaveLength(2);
  });

  it("matches on record id, prompt id, name, description and content", () => {
    expect(filterPromptEntries(entries, "p2")).toHaveLength(1);
    expect(filterPromptEntries(entries, "beta")).toHaveLength(1);
    expect(filterPromptEntries(entries, "NEEDLE")).toHaveLength(1);
  });

  it("returns nothing when no field matches", () => {
    expect(filterPromptEntries(entries, "gamma")).toHaveLength(0);
  });
});

describe("PromptLibrary", () => {
  it("renders one row per prompt", () => {
    render(
      <PromptLibrary
        {...libraryProps}
        prompts={{ p1: prompt({ id: "p1" }), p2: prompt({ id: "p2" }) }}
        searchQuery=""
      />,
    );
    expect(screen.getByTestId("prompt-row-p1")).toBeInTheDocument();
    expect(screen.getByTestId("prompt-row-p2")).toBeInTheDocument();
    // 两条目同名，用 getAllByText 而非放宽断言强度
    expect(screen.getAllByText("Alpha")).toHaveLength(2);
  });

  it("hides rows that do not match the query", () => {
    render(
      <PromptLibrary
        {...libraryProps}
        prompts={{
          p1: prompt({ id: "p1", name: "alpha" }),
          p2: prompt({ id: "p2", name: "beta" }),
        }}
        searchQuery="beta"
      />,
    );
    expect(screen.queryByTestId("prompt-row-p1")).not.toBeInTheDocument();
    expect(screen.getByTestId("prompt-row-p2")).toBeInTheDocument();
  });

  it("offers a way out of an empty result set", async () => {
    const onClearSearch = vi.fn();
    render(
      <PromptLibrary
        {...libraryProps}
        onClearSearch={onClearSearch}
        prompts={{ p1: prompt({ id: "p1" }) }}
        searchQuery="zzz"
      />,
    );
    expect(screen.queryByTestId("prompt-row-p1")).not.toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "prompts.clearSearch" }),
    );
    expect(onClearSearch).toHaveBeenCalledTimes(1);
  });

  it("wires every row action to its handler with the row id", async () => {
    const handlers = {
      onToggle: vi.fn(),
      onEdit: vi.fn(),
      onDelete: vi.fn(),
      onCopyToApps: vi.fn(),
      onCopyContent: vi.fn(),
    };
    render(
      <PromptLibrary
        {...libraryProps}
        {...handlers}
        prompts={{ p1: prompt({ id: "p1", enabled: false }) }}
        searchQuery=""
      />,
    );
    const row = within(screen.getByTestId("prompt-row-p1"));
    await userEvent.click(row.getByRole("button", { name: "toggle" }));
    expect(handlers.onToggle).toHaveBeenCalledWith("p1", true);
    await userEvent.click(row.getByRole("button", { name: "edit" }));
    expect(handlers.onEdit).toHaveBeenCalledWith("p1");
    await userEvent.click(row.getByRole("button", { name: "delete" }));
    expect(handlers.onDelete).toHaveBeenCalledWith("p1");
    await userEvent.click(row.getByRole("button", { name: "copy-apps" }));
    expect(handlers.onCopyToApps).toHaveBeenCalledWith("p1");
    await userEvent.click(row.getByRole("button", { name: "copy-content" }));
    expect(handlers.onCopyContent).toHaveBeenCalledWith("p1");
  });

  // 渲染基准：搜索词变化（父组件状态变化）时，仍然匹配的行既不得重新渲染，
  // 也不得重算 detail——detail 含对整篇正文的 utf8Bytes/firstLine 扫描。
  // harness 的 handler 与修好之后的 PromptPanel 一致：渲染间保持同一身份。
  it("keeps surviving rows render-free and detail-free when the query changes (row memo baseline)", () => {
    const prompts = {
      p1: prompt({ id: "p1", name: "alpha one", content: "one" }),
      p2: prompt({ id: "p2", name: "alpha two", content: "two" }),
      p3: prompt({ id: "p3", name: "alpha three", content: "three" }),
    };
    const Harness = ({ query }: { query: string }) => (
      <PromptLibrary {...libraryProps} prompts={prompts} searchQuery={query} />
    );

    const { rerender } = render(<Harness query="" />);
    const rowCalls = promptRowSpy.mock.calls.length;
    const detailCalls = utf8BytesSpy.mock.calls.length;
    expect(rowCalls).toBe(3);
    // detail 改由行组件自己算：列表的 map 一次都不该扫正文。
    expect(detailCalls).toBe(0);

    // rerender 每次构造新 element：复用同一引用时 React 整棵 bailout，基准失效。
    rerender(<Harness query="alpha" />);

    expect(promptRowSpy.mock.calls.length).toBe(rowCalls);
    expect(utf8BytesSpy.mock.calls.length).toBe(detailCalls);
    expect(firstLineSpy.mock.calls.length).toBe(0);
    expect(formatUpdatedSpy.mock.calls.length).toBe(0);
  });

  // 渲染基准：与搜索无关的父组件重渲染（例如复制对话框开合）同样不得
  // 触碰列表。
  it("keeps the list untouched when the parent re-renders without new props (section memo baseline)", () => {
    const prompts = {
      p1: prompt({ id: "p1", name: "alpha one", content: "one" }),
      p2: prompt({ id: "p2", name: "alpha two", content: "two" }),
    };
    const Harness = () => (
      <PromptLibrary {...libraryProps} prompts={prompts} searchQuery="" />
    );

    const { rerender } = render(<Harness />);
    const rowCalls = promptRowSpy.mock.calls.length;
    const detailCalls = utf8BytesSpy.mock.calls.length;
    expect(rowCalls).toBe(2);
    expect(detailCalls).toBe(0);

    rerender(<Harness />);

    expect(promptRowSpy.mock.calls.length).toBe(rowCalls);
    expect(utf8BytesSpy.mock.calls.length).toBe(detailCalls);
  });
});
