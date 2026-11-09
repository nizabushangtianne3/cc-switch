import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, act } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Sidebar } from "@/components/shell/Sidebar";
import type { Settings, VisibleApps } from "@/types";
import { createTestQueryClient } from "../../utils/testQueryClient";

// 用 useSyncExternalStore 忠实模拟 react-query 的订阅：poll 拿到新数据时，
// 订阅方组件重渲染，和线上每 2 秒的代理状态轮询是同一条路径。
const sidebarStatus = vi.hoisted(() => {
  let state = {
    modes: {} as Record<string, string>,
    todayCost: 10,
    todayTokens: 0,
    todayRequests: 0,
    todayHitRate: 0,
    authNeedsAttention: false,
  };
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set: (next: Partial<typeof state>) => {
      state = { ...state, ...next };
      listeners.forEach((listener) => listener());
    },
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
});

const appStatusSpy = vi.hoisted(() =>
  vi.fn((_app: string) => ({ mode: "direct", alert: false })),
);

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { value?: string }) =>
      options?.value != null ? `${key}:${options.value}` : key,
    i18n: { resolvedLanguage: "en" },
  }),
}));

vi.mock("@/contexts/UpdateContext", () => ({
  useUpdate: () => ({ hasUpdate: false, updateInfo: null }),
}));

vi.mock("@/hooks/useSidebarStatus", async () => {
  const { useSyncExternalStore, useMemo } =
    await vi.importActual<typeof import("react")>("react");
  return {
    useSidebarStatus: () => {
      const state = useSyncExternalStore(
        sidebarStatus.subscribe,
        sidebarStatus.get,
        sidebarStatus.get,
      );
      // 真 hook 的语义：appStatus 身份随各应用模式变化，数据没变时保持稳定。
      // 少了这条，基准测到的只是「另一种 churn」。
      const modesKey = JSON.stringify(state.modes);
      const appStatus = useMemo(
        () => (app: string) =>
          appStatusSpy(app) ?? {
            mode: state.modes[app] ?? "direct",
            alert: false,
          },
        // eslint-disable-next-line react-hooks/exhaustive-deps
        [modesKey],
      );
      return {
        ...state,
        appStatus,
      };
    },
  };
});

const ALL_VISIBLE: VisibleApps = {
  claude: true,
  "claude-desktop": true,
  codex: true,
  gemini: true,
  grokbuild: true,
  opencode: true,
  openclaw: true,
  hermes: true,
  pi: true,
  kimi: true,
  dsh: true,
  mcode: true,
  agy: true,
  omp: true,
  copilot: true,
};

const renderSidebar = () => {
  const client = createTestQueryClient();
  client.setQueryDefaults(["settings"], { staleTime: Infinity });
  client.setQueryData(["settings"], {
    usageDisplay: { sidebarTodayItems: ["cost"] },
  } as Partial<Settings>);
  return render(
    <QueryClientProvider client={client}>
      <Sidebar
        activeApp="claude"
        view="usage"
        visibleApps={ALL_VISIBLE}
        settingsSection="general"
        onSelectApp={vi.fn()}
        onSelectPage={vi.fn()}
        onSelectSettingsSection={vi.fn()}
        onExitSettings={vi.fn()}
      />
    </QueryClientProvider>,
  );
};

describe("sidebar nav render baseline", () => {
  it("re-runs no app row when an unrelated status field changes (nav memo baseline)", () => {
    renderSidebar();
    const appRows = appStatusSpy.mock.calls.length;
    // 前提自断言：应用行确实渲染了，否则基准什么都没测
    expect(appRows).toBe(15);

    // 一次轮询：只有今日花费变了，应用接管状态一个都没变
    act(() => {
      sidebarStatus.set({ todayCost: 20 });
    });

    // 重渲染真的发生了（今日标签换了数），但应用行的 map 体不得重跑
    expect(screen.getByText("nav.today:$20.00")).toBeInTheDocument();
    expect(appStatusSpy.mock.calls.length).toBe(appRows);
  });

  it("still re-runs the app rows when an app mode really changes", () => {
    renderSidebar();
    const appRows = appStatusSpy.mock.calls.length;

    act(() => {
      sidebarStatus.set({ modes: { claude: "route" } });
    });

    // 模式真的变了：行必须重跑，否则角标不更新
    expect(appStatusSpy.mock.calls.length).toBe(appRows + 15);
  });
});
