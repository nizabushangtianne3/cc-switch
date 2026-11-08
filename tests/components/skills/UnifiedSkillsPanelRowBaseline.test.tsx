import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi, beforeEach } from "vitest";

import UnifiedSkillsPanel from "@/components/skills/UnifiedSkillsPanel";
import type { InstalledSkill } from "@/lib/api/skills";

const m = vi.hoisted(() => ({
  // mutation 对象必须跨渲染稳定：生产里 react-query 的 mutation 实例就是
  // 稳定的，mock 每次新建会让所有依赖它的 useCallback 变身份。
  toggleAppMutation: {
    mutateAsync: vi.fn(async () => ({})),
    isPending: false,
  },
  bulkToggleAppMutation: {
    mutateAsync: vi.fn(async () => ({ succeeded: [], failed: [] })),
    isPending: false,
  },
  uninstallMutation: {
    mutateAsync: vi.fn(async () => ({})),
    isPending: false,
  },
  updateSkillMutation: {
    mutateAsync: vi.fn(async () => ({})),
    isPending: false,
  },
  installed: [] as InstalledSkill[],
  backups: [] as unknown[],
  updates: [] as unknown[],
  repoFailures: [] as unknown[],
  checking: false,
  visibleApps: ["claude", "codex", "pi"] as string[],
}));

// 行渲染探针：每行每次渲染必调一次 t("skillsPage.selectAria", { name })，
// 用它精确数「InstalledRow 渲染了几次」。
const rowRenderSpy = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", async () => {
  const actual =
    await vi.importActual<typeof import("react-i18next")>("react-i18next");
  // 生产里 t 在同一语言下是稳定引用；这里包一层计数就必须缓存，
  // 否则每个依赖 t 的 useCallback 都会变，基准测的是基建而不是代码。
  let cachedT:
    | ((key: string, options?: Record<string, unknown>) => unknown)
    | null = null;
  return {
    ...actual,
    useTranslation: () => {
      const real = actual.useTranslation();
      if (!cachedT) {
        cachedT = (key: string, options?: Record<string, unknown>) => {
          if (key === "skillsPage.selectAria") rowRenderSpy(options?.name);
          return real.t(key, options as never);
        };
      }
      return {
        ...real,
        t: cachedT as typeof real.t,
      };
    },
  };
});

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@/components/mcp/useVisibleAppIds", async () => {
  const { useMemo } = await vi.importActual<typeof import("react")>("react");
  return {
    // 真 hook 里是 useMemo；mock 不 memo 会让 appIds 每次都是新数组，
    // 行的 memo 被一个测试基建问题破功，基准就测不到真实契约。
    useVisibleAppIds: (ids: string[]) => {
      const visible = m.visibleApps;
      return useMemo(
        () => ids.filter((id) => visible.includes(id)),
        [ids, visible],
      );
    },
  };
});

vi.mock("@/components/skills/SkillsStorageSheet", () => ({
  SkillsStorageSheet: ({ open }: { open: boolean }) =>
    open ? <div data-testid="storage-sheet" /> : null,
}));

vi.mock("@/hooks/useSkills", () => ({
  useInstalledSkills: () => ({
    data: m.installed,
    isLoading: false,
    isError: false,
    refetch: vi.fn(),
  }),
  useSkillBackups: () => ({
    data: m.backups,
    refetch: vi.fn(),
    isFetching: false,
  }),
  useDeleteSkillBackup: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useToggleSkillApp: () => m.toggleAppMutation,
  useBulkToggleSkillApp: () => ({
    mutateAsync: vi.fn(async ({ ids }: { ids: string[] }) => ({
      succeeded: ids,
      failed: [],
    })),
    isPending: false,
  }),
  useUpdateSkill: () => m.updateSkillMutation,
  useRestoreSkillBackup: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useUninstallSkill: () => m.uninstallMutation,
  useScanUnmanagedSkills: () => ({ data: [], refetch: vi.fn() }),
  useImportSkillsFromApps: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useInstallSkillsFromZip: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useCheckSkillUpdates: () => ({
    data: { updates: m.updates, failures: m.repoFailures },
    refetch: vi.fn(),
    isFetching: m.checking,
    dataUpdatedAt: 0,
  }),
  useDiscoverableSkills: () => ({ data: [], refetch: vi.fn() }),
  useRefreshDiscoverableSkills: () => vi.fn(),
  useDiscoverableSkillsFailures: () => ({ data: [] }),
  useSkillRepos: () => ({ data: [], refetch: vi.fn() }),
  useAddSkillRepo: () => ({ mutateAsync: vi.fn() }),
  useRemoveSkillRepo: () => ({ mutateAsync: vi.fn() }),
  useSearchSkillsSh: () => ({ data: undefined, isLoading: false }),
  useInstallSkill: () => ({ mutateAsync: vi.fn() }),
}));

const makeSkill = (name: string, id: string): InstalledSkill => ({
  id,
  name,
  description: `${name} description`,
  directory: id,
  repoOwner: "owner",
  repoName: "repo",
  repoBranch: "main",
  apps: {
    claude: false,
    codex: false,
    gemini: false,
    grokbuild: false,
    opencode: false,
    openclaw: false,
    hermes: false,
    pi: false,
  },
  installedAt: 1,
  updatedAt: 1,
});

const searchBox = () =>
  screen.getByRole("textbox", { name: "skills.installedSearchAriaLabel" });

describe("skills row render baseline", () => {
  beforeEach(() => {
    m.installed = [];
    m.visibleApps = ["claude", "codex", "pi"];
    rowRenderSpy.mockClear();
  });

  // 搜索词每变一个字符，面板就重渲染一次。仍然匹配的行必须整体 bail out：
  // 每行 8 个内联回调 + 整棵行的 DOM 都不该重建。
  it("keeps surviving rows render-free while typing in the search box (row memo baseline)", async () => {
    m.installed = [
      makeSkill("alpha one", "id-1"),
      makeSkill("alpha two", "id-2"),
      makeSkill("alpha three", "id-3"),
      makeSkill("alpha four", "id-4"),
      makeSkill("alpha five", "id-5"),
    ];
    render(<UnifiedSkillsPanel />);
    const rowRenders = rowRenderSpy.mock.calls.length;
    // 前提自断言：5 行各渲染一次
    expect(rowRenders).toBe(5);

    // 5 条都含 "alpha"：输入后过滤结果不变，变的只有面板自身状态
    await userEvent.type(searchBox(), "alpha");

    // 行一个都不该重渲染
    expect(rowRenderSpy.mock.calls.length).toBe(rowRenders);
  });

  // 反向断言：行自己的状态变了必须重渲染，否则勾选态不更新。
  it("re-renders only the row whose own selection changed", async () => {
    m.installed = [
      makeSkill("alpha one", "id-1"),
      makeSkill("alpha two", "id-2"),
      makeSkill("alpha three", "id-3"),
    ];
    render(<UnifiedSkillsPanel />);
    const rendersOf = (name: string) =>
      rowRenderSpy.mock.calls.filter((call) => call[0] === name).length;
    expect(rendersOf("alpha one")).toBe(1);
    expect(rendersOf("alpha two")).toBe(1);
    expect(rendersOf("alpha three")).toBe(1);

    // 索引 0 是工具栏的「全选」，行从 1 开始：取第二行 alpha two
    const checkboxes = screen.getAllByRole("checkbox");
    await userEvent.click(checkboxes[2]);

    // 被点的那一行重渲染，另外两行一次都不许多
    expect(rendersOf("alpha two")).toBeGreaterThan(1);
    expect(rendersOf("alpha one")).toBe(1);
    expect(rendersOf("alpha three")).toBe(1);
  });
});
