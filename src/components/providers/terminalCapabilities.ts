import { resolveManagedAccountId } from "@/lib/authBinding";
import { providerNeedsRouting } from "@/utils/providerCapabilities";
import type { AppId } from "@/lib/api/types";
import type { Provider } from "@/types";

/** Codex 卡自己带路由表（第三方地址），终端能只对这一次用 `-c` 换成它 */
function codexHasOwnRoute(provider: Provider): boolean {
  if (provider.category === "official") return false;
  if (providerNeedsRouting("codex", provider)) return false;
  const config = (provider.settingsConfig as { config?: unknown } | undefined)
    ?.config;
  return typeof config === "string" && /^\s*base_url\s*=/m.test(config);
}

/**
 * 卡片上显示不显示「打开终端」。Codex 的官方卡、没有路由的卡跟着 Codex 自己的登录走，
 * 只有是当前供应商时才能打开（直接跑 `codex`）；要经本地路由的卡终端直连不了。
 */
export function terminalAvailable(
  provider: Provider,
  appId: AppId,
  isCurrent: boolean,
): boolean {
  if (appId !== "codex") return true;
  if (providerNeedsRouting("codex", provider)) return false;
  return isCurrent || codexHasOwnRoute(provider);
}

/**
 * Claude Code：终端经 `claude --settings <临时文件>` 启动，临时文件只放这张卡的 env，没写的
 * 键仍从 ~/.claude/settings.json 读。卡上同时有地址和凭据才压得住全局配置。官方卡绑了授权
 * 中心的账号时，终端用这个账号的 `CLAUDE_CONFIG_DIR` 启动（后端 `claude_official::
 * terminal_account`），也固定在这个号上；没绑的官方卡 env 为空，跟着全局登录走，不能说它隔离。
 *
 * Codex：带路由表的第三方卡经 `codex -c` 只对这个进程换成它，Key 经环境变量传，不写
 * config.toml（后端 `codex_terminal`）；官方卡跟着 Codex 自己的登录走。
 *
 * Antigravity：带 Key 的卡用自己的私有 `--gemini_dir` 启动，Key 只给这个 agy 进程；官方
 * 登录卡直接跑原生 agy，跟着全局设置和登录。
 */
export function terminalIsIsolated(
  provider: Provider,
  appId: AppId = "claude",
): boolean {
  if (appId === "codex") return codexHasOwnRoute(provider);
  const env = (provider.settingsConfig as { env?: unknown } | undefined)?.env;
  if (!env || typeof env !== "object") return false;
  const filled = (key: string) => {
    const value = (env as Record<string, unknown>)[key];
    return typeof value === "string" && value.trim() !== "";
  };
  if (appId === "agy") {
    return provider.category !== "official" && filled("GEMINI_API_KEY");
  }
  if (provider.category === "official") {
    return Boolean(
      resolveManagedAccountId(provider.meta, "claude_official")?.trim(),
    );
  }
  return (
    filled("ANTHROPIC_BASE_URL") &&
    (filled("ANTHROPIC_AUTH_TOKEN") || filled("ANTHROPIC_API_KEY"))
  );
}
