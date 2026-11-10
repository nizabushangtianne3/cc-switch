import { useSyncExternalStore } from "react";
import type { NodeRequirementGap } from "@/lib/api/settings";
import { settingsApi } from "@/lib/api";
import { isUpdateAvailable } from "@/lib/version";

/**
 * 命令行工具的版本探测结果，以及「启动时后台查一次」这件事本身。
 *
 * 单独成模块的原因：入口（App.tsx）只需要「启动时查一次」和「有没有可升级」，
 * 而它们只依赖这张版本表。「应用」页的安装 / 升级 / 卸载逻辑在
 * `./useToolManagement`，入口不碰——那会让整页逻辑（1 千多行）进首屏包。
 */

export const TOOL_NAMES = [
  "claude",
  "codex",
  "gemini",
  "grok",
  "opencode",
  "openclaw",
  "hermes",
  "pi",
  "mcode",
  "kimi",
  "dsh",
  "agy",
  "omp",
  "copilot",
] as const;
export type ToolName = (typeof TOOL_NAMES)[number];

export interface ToolVersion {
  name: string;
  version: string | null;
  latest_version: string | null;
  error: string | null;
  // 后端已定位到可执行文件但 --version 报错（装了却跑不起来）。直接读此字段，
  // 不要靠匹配 error 文案反推——避免前端与后端字符串硬耦合。
  installed_but_broken: boolean;
  env_type: "windows" | "wsl" | "macos" | "linux" | "unknown";
  wsl_distro: string | null;
  // 有新版本、但这处安装用的 Node 达不到新版本的要求：不给升级，提示先升级 Node。
  update_blocked_by_node: NodeRequirementGap | null;
}

export type ToolVersionsCache = { data: ToolVersion[]; at: number } | null;

// 工具版本探测代价高：每个工具一次 `--version` 子进程 + 一次 npm/github/pypi 网络请求。
// 「应用」页离开就卸载，每次回来都重挂，若都全量重查纯属浪费。用「模块级」缓存（生命周期
// = JS 模块 = 应用会话，不随组件卸载销毁）跨重挂存活：重挂时若缓存仍新鲜（距上次全量加载
// < TTL）直接复用、跳过探测；超期或用户手动「刷新」才强制重查。at = 最近一次「全量加载」
// 完成时刻；单工具刷新（切 shell / 升级后）只更新数据、不重置 at，避免一次局部刷新把整体
// TTL 续命。
const TOOL_VERSIONS_CACHE_TTL_MS = 10 * 60 * 1000; // 10 分钟
const EMPTY_TOOL_VERSIONS: ToolVersion[] = [];

let toolVersionsCache: ToolVersionsCache = null;
const toolVersionsListeners = new Set<() => void>();

const getToolVersionsCache = () => toolVersionsCache;
function subscribeToolVersions(listener: () => void) {
  toolVersionsListeners.add(listener);
  return () => {
    toolVersionsListeners.delete(listener);
  };
}
export function setToolVersionsCache(cache: ToolVersionsCache) {
  toolVersionsCache = cache;
  toolVersionsListeners.forEach((listener) => listener());
}

/** React 用的版本缓存订阅。 */
export function useToolVersionsCache(): ToolVersionsCache {
  return useSyncExternalStore(subscribeToolVersions, getToolVersionsCache);
}

/** 非 React 场景（页面副作用、回调里）同步读当前缓存。 */
export const currentToolVersionsCache = () => toolVersionsCache;

/** 全量加载的时间戳还算不算新鲜。 */
export const toolVersionsCacheIsFresh = (at: number) =>
  Date.now() - at < TOOL_VERSIONS_CACHE_TTL_MS;

/** 把探测结果按 name 合并进已有列表：替换同名项、追加新项；空列表时直接采用新结果。 */
export function mergeToolVersions(
  prev: ToolVersion[],
  updated: ToolVersion[],
): ToolVersion[] {
  if (prev.length === 0) return updated;
  const byName = new Map(updated.map((t) => [t.name, t]));
  const merged = prev.map((t) => byName.get(t.name) ?? t);
  const existing = new Set(prev.map((t) => t.name));
  for (const u of updated) {
    if (!existing.has(u.name)) merged.push(u);
  }
  return merged;
}

// 请求顺序跨组件挂载保留，旧页面的迟到响应不能覆盖后续刷新或升级结果。
let toolVersionRequestSequence = 0;
const latestToolVersionRequests = new Map<string, number>();

/** 认领一批工具的探测请求，返回本次的请求号。 */
export function beginToolVersionRequests(names: readonly string[]): number {
  const requestId = ++toolVersionRequestSequence;
  names.forEach((name) => latestToolVersionRequests.set(name, requestId));
  return requestId;
}

/** 这个工具的响应还是最新一次请求的吗（迟到的旧响应要丢掉）。 */
export const isLatestToolVersionRequest = (name: string, requestId: number) =>
  latestToolVersionRequests.get(name) === requestId;

/**
 * 设置里打开了「启动时检查应用更新」时，启动后在后台查一次（结果进同一个模块缓存）。
 * 不弹任何提示，只让侧栏「应用」上出现圆点。
 */
export async function checkToolUpdatesInBackground(): Promise<void> {
  const cache = currentToolVersionsCache();
  if (cache && Date.now() - cache.at < TOOL_VERSIONS_CACHE_TTL_MS) return;
  const requestId = beginToolVersionRequests(TOOL_NAMES);
  try {
    const results = await settingsApi.getToolVersions([...TOOL_NAMES]);
    const current = results.filter((tool) =>
      isLatestToolVersionRequest(tool.name, requestId),
    );
    if (current.length === 0) return;
    const latest = currentToolVersionsCache();
    setToolVersionsCache({
      data: mergeToolVersions(latest?.data ?? [], current),
      at: Date.now(),
    });
  } catch (error) {
    console.error("[useToolUpdates] Background update check failed", error);
  }
}

/** 已检查过的命令行应用里有没有可升级的（不触发检查，只读缓存）。 */
export function useToolUpdatesAvailable(): boolean {
  const cache = useToolVersionsCache();
  return (
    cache?.data.some((tool) =>
      isUpdateAvailable(tool.version, tool.latest_version),
    ) ?? false
  );
}

/** 页面首挂时的稳定空数组：缓存还没落地时不要每个渲染都建新引用。 */
export const emptyToolVersions = () => EMPTY_TOOL_VERSIONS;
