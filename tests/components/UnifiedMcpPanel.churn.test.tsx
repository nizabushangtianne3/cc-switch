import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import i18n from "i18next";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import UnifiedMcpPanel from "@/components/mcp/UnifiedMcpPanel";
import type { McpApps, McpServer, McpServerSpec } from "@/types";

const mocks = vi.hoisted(() => ({
  serversMap: {} as Record<string, McpServer>,
  isLoading: false,
  isError: false,
  toggle: vi.fn(),
  bulkToggle: vi.fn(),
  deleteServer: vi.fn(),
  importServers: vi.fn(),
  resync: vi.fn(),
  refetch: vi.fn(),
  toastError: vi.fn(),
  toastSuccess: vi.fn(),
  toastWarning: vi.fn(),
  visibleApps: ["claude", "codex", "gemini"] as string[],
  visibleAppIdsCache: null as string[] | null,
  connectivity: {
    states: {} as Record<string, string>,
    checking: false,
    canCheck: true,
    checkAll: vi.fn(),
    checkServer: vi.fn(),
  },
}));

vi.mock("@/hooks/useMcp", () => ({
  MCP_UPSERT_MUTATION_KEY: ["mcp", "upsert"],
  useAllMcpServers: () => ({
    data: mocks.serversMap,
    isLoading: mocks.isLoading,
    isError: mocks.isError,
    error: mocks.isError ? new Error("database is locked") : null,
    refetch: mocks.refetch,
  }),
  useToggleMcpApp: () => ({ mutateAsync: mocks.toggle, isPending: false }),
  useBulkToggleMcpApp: () => ({
    mutateAsync: mocks.bulkToggle,
    isPending: false,
  }),
  useDeleteMcpServer: () => ({
    mutateAsync: mocks.deleteServer,
    isPending: false,
  }),
  useImportMcpFromApps: () => ({
    mutateAsync: mocks.importServers,
    isPending: false,
  }),
  useResyncMcpToApps: () => ({
    mutateAsync: mocks.resync,
    isPending: false,
  }),
}));

vi.mock("@/components/mcp/useVisibleAppIds", () => ({
  // 真实 hook 是 useMemo 化的（[appIds, visibleApps]）；mock 每次渲染新建数组会把
  // 行的 memo 打穿，这里缓存一份对齐生产行为。
  useVisibleAppIds: (ids: string[]) => {
    if (!mocks.visibleAppIdsCache) {
      mocks.visibleAppIdsCache = ids.filter((id) =>
        mocks.visibleApps.includes(id),
      );
    }
    return mocks.visibleAppIdsCache;
  },
}));

vi.mock("@/hooks/useMcpConnectivity", () => ({
  useMcpConnectivity: () => mocks.connectivity,
}));

vi.mock("@/components/mcp/McpFormModal", () => ({
  default: ({ editingId }: { editingId?: string }) => (
    <div data-testid="mcp-drawer">{editingId ?? "add"}</div>
  ),
}));

vi.mock("sonner", () => ({
  toast: {
    error: mocks.toastError,
    success: mocks.toastSuccess,
    warning: mocks.toastWarning,
    info: vi.fn(),
  },
}));

const matrixCellSpy = vi.fn();

// 计数探针：每行每个应用一个格子。包 memo，否则父组件重渲染必然调用它，基准测不到
// memo 契约。
vi.mock("@/components/mcp/AppMatrix", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/components/mcp/AppMatrix")>();
  const { memo } = await vi.importActual<typeof import("react")>("react");
  return {
    ...actual,
    MatrixCell: memo((props: Record<string, unknown>) => {
      matrixCellSpy(props);
      return (
        <button
          type="button"
          aria-label={props.label as string}
          disabled={props.disabled as boolean}
        />
      );
    }),
  };
});

function makeServer(id: string, apps: Partial<McpApps>): McpServer {
  return {
    id,
    name: id,
    server: { type: "stdio", command: "cmd" } as McpServerSpec,
    apps: {
      claude: false,
      codex: false,
      gemini: false,
      grokbuild: false,
      opencode: false,
      openclaw: false,
      hermes: false,
      ...apps,
    },
  } as McpServer;
}

const enabledApps = { claude: true, codex: true, gemini: true };

beforeAll(() => {
  // 全局 setup 的语言包是空的，t() 不插值。给格子 label 补一份含 {{name}} 的资源，
  // 探针才能按行 identity 数渲染次数。
  i18n.addResourceBundle(
    "zh",
    "translation",
    {
      appMatrix: {
        cell: {
          on: "{{name}}|{{app}}|on",
          off: "{{name}}|{{app}}|off",
          fail: "{{name}}|{{app}}|fail",
        },
      },
    },
    true,
    true,
  );
});

describe("UnifiedMcpPanel render churn", () => {
  beforeEach(() => {
    mocks.visibleAppIdsCache = null;
    mocks.serversMap = {
      "alpha-one": makeServer("alpha-one", enabledApps),
      "alpha-two": makeServer("alpha-two", enabledApps),
      "beta-one": makeServer("beta-one", enabledApps),
    };
    mocks.isLoading = false;
    mocks.isError = false;
    mocks.visibleApps = ["claude", "codex", "gemini"];
  });

  it("typing in the search box leaves the surviving rows untouched", async () => {
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={new QueryClient()}>
        <UnifiedMcpPanel />
      </QueryClientProvider>,
    );

    // 三家都在列表里，搜索框键入后三家仍全部命中（名字都以 alpha/beta 开头，
    // 查询词对三行一视同仁）：行数据没变，memo 化的行渲染次数不应增长。
    const rendersFor = (labelPart: string) =>
      matrixCellSpy.mock.calls.filter(([props]) =>
        String(props.label).includes(labelPart),
      ).length;

    const betaAtMount = rendersFor("beta-one");
    expect(betaAtMount).toBeGreaterThan(0);

    await user.type(screen.getByRole("textbox"), "a");

    expect(rendersFor("beta-one")).toBe(betaAtMount);
  });
});
