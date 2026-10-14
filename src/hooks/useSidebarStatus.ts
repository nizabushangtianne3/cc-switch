import { useMemo } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import type { AppId } from "@/lib/api";
import * as authApi from "@/lib/api/auth";
import { useProvidersQuery } from "@/lib/query";
import { useProxyStatusQuery } from "@/lib/query/proxy";
import { proxyApi } from "@/lib/api/proxy";
import { useUsageSummary } from "@/lib/query/usage";
import { PROXY_APP_IDS, isProxyAppId } from "@/config/appConfig";
import { providerNeedsRouting } from "@/utils/providerCapabilities";
import { parseFiniteNumber } from "@/components/usage/format";
import type { UsageRangeSelection } from "@/types/usage";
import type { AppMode } from "@/types/proxy";

export interface AppNavStatus {
  mode: AppMode;
  /** Claude Desktop 的当前供应商走模型映射（需要路由服务） */
  mapping: boolean;
  /** 需要处理：在路由 / 聚合 / 映射，但路由服务没在运行 */
  alert: boolean;
}

const TODAY: UsageRangeSelection = { preset: "today" };

/**
 * 侧栏需要的状态：每个应用的模式标签和提醒、今日花费、授权中心是否有账号要重新登录。
 */
export function useSidebarStatus() {
  const { data: proxyStatus } = useProxyStatusQuery();
  // 和应用页的模式行同一份数据（["providers", app, "mode"]），进出模式时一起刷新
  const modeQueries = useQueries({
    queries: PROXY_APP_IDS.map((app) => ({
      queryKey: ["providers", app, "mode"],
      queryFn: () => proxyApi.getAppMode(app),
    })),
  });
  const { data: desktopProviders } = useProvidersQuery("claude-desktop");
  const { data: todaySummary } = useUsageSummary(TODAY, undefined, {
    refetchInterval: 60_000,
  });

  const { data: authStatuses } = useQuery({
    queryKey: ["managed-auth-status"],
    queryFn: authApi.authGetStatuses,
    staleTime: 60_000,
  });

  const serviceRunning = proxyStatus?.running ?? false;
  const modeKey = PROXY_APP_IDS.map(
    (_, index) => modeQueries[index]?.data?.mode ?? "direct",
  ).join(",");
  const modeByApp = useMemo(() => {
    const modes = modeKey.split(",") as AppMode[];
    return new Map(
      PROXY_APP_IDS.map((app, index) => [app, modes[index] ?? "direct"]),
    );
  }, [modeKey]);

  const desktopMapping = useMemo(() => {
    const current =
      desktopProviders?.providers[desktopProviders.currentProviderId];
    return current ? providerNeedsRouting("claude-desktop", current) : false;
  }, [desktopProviders]);

  const appStatus = useMemo(() => {
    return (app: AppId): AppNavStatus => {
      if (app === "claude-desktop") {
        return {
          mode: "direct",
          mapping: desktopMapping,
          alert: desktopMapping && proxyStatus !== undefined && !serviceRunning,
        };
      }
      if (!isProxyAppId(app)) {
        return { mode: "direct", mapping: false, alert: false };
      }
      const mode = modeByApp.get(app) ?? "direct";
      return {
        mode,
        mapping: false,
        alert:
          mode !== "direct" && proxyStatus !== undefined && !serviceRunning,
      };
    };
  }, [desktopMapping, proxyStatus, serviceRunning, modeByApp]);

  const todayCost = parseFiniteNumber(todaySummary?.totalCost) ?? 0;

  const authNeedsAttention = Object.values(authStatuses ?? {}).some((status) =>
    status.accounts.some(
      (account) => account.requires_reauth || account.reauth_required === true,
    ),
  );

  return { appStatus, todayCost, authNeedsAttention };
}
