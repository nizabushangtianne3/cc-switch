import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";
import { authApi } from "@/lib/api";
import { claudePlanLabel } from "@/lib/claudePlan";

const LOGIN_SWITCH_KEY = ["claude-login-switch"] as const;

function useClaudeLoginSwitch() {
  return useQuery({
    queryKey: LOGIN_SWITCH_KEY,
    queryFn: () => authApi.getClaudeLoginSwitch(),
    staleTime: 30000,
  });
}

/**
 * 切卡为什么不会换 Claude Code 的登录：
 * - `"wsl"`：配置目录在 WSL 里，账号功能整体不可用（登录、终端都跑在 Windows 上）；
 * - `"overrideDir"`：设了别的配置目录覆盖，只能从卡片「打开终端」用选中账号。
 */
/** ClaudeOfficialSection 的账号选择也要用它提示「为什么切卡不换号」，单一定义放这里。 */
export function useClaudeLoginBlocked(): "wsl" | "overrideDir" | null {
  const { data } = useClaudeLoginSwitch();
  if (data?.active !== false) return null;
  return data.blocked === "wsl" ? "wsl" : "overrideDir";
}

function useClaudeAccount(accountId: string) {
  const { data, isSuccess } = useQuery({
    queryKey: ["managed-auth-status", "claude_official"],
    queryFn: () => authApi.authGetStatus("claude_official"),
    staleTime: 30000,
  });
  return {
    isSuccess,
    account: data?.accounts.find((a) => a.id === accountId),
  };
}

/**
 * 绑了账号的 Claude 官方卡副标题（官网地址的位置），和 Codex 官方卡同一套写法：写账号，
 * 有备注写备注，卡名里已经有账号就写「Claude 账号」；登录失效、或切卡不会换号（设了配置
 * 目录覆盖）时跟一句提示。
 */
export function ClaudeBoundAccountLine({
  accountId,
  providerName,
  note,
  showPlan = false,
}: {
  accountId: string;
  providerName: string;
  note?: string;
  /** 卡片开了额度查询时才写套餐 */
  showPlan?: boolean;
}) {
  const { t } = useTranslation();
  const blocked = useClaudeLoginBlocked();
  const { isSuccess, account } = useClaudeAccount(accountId);
  if (!isSuccess) return null;
  if (!account) {
    return (
      <div className="mt-0.5 flex min-w-0 items-center gap-1 text-caption text-warning-text">
        <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
        <span className="min-w-0 truncate">
          {t("provider.form.claudeAccount.missing", {
            defaultValue: "不在这台电脑上的账号",
          })}
        </span>
      </div>
    );
  }
  const expiresAt = account.login_expires_at ?? null;
  const needsReauth =
    account.requires_reauth || (expiresAt !== null && expiresAt <= Date.now());
  const plan = showPlan
    ? claudePlanLabel(t, account.plan, account.rate_limit_tier)
    : null;
  const name = providerName.trim();
  const label =
    note ??
    (name === account.login || name.includes(account.login)
      ? t("provider.form.claudeAccount.claudeAccount", {
          defaultValue: "Claude 账号",
        })
      : account.login);
  return (
    <div className="mt-0.5 flex min-w-0 items-center gap-2 text-caption text-fg-2">
      <span className="min-w-0 truncate" title={note ?? account.login}>
        {label}
      </span>
      {/* 跟在账号后面：「me@example.com · Max 20x」（抵掉一半间距，和「 · 」一样宽） */}
      {plan && <span className="-ms-1 shrink-0">· {plan}</span>}
      {needsReauth ? (
        <span className="inline-flex shrink-0 items-center gap-1 text-warning-text">
          <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
          {t("provider.form.claudeAccount.reauthBadge", {
            defaultValue: "需要重新登录",
          })}
        </span>
      ) : blocked === "wsl" ? (
        <span className="shrink-0">
          {t("provider.form.claudeAccount.wslHint", {
            defaultValue: "WSL 暂不支持",
          })}
        </span>
      ) : blocked === "overrideDir" ? (
        <span className="shrink-0">
          {t("provider.form.claudeAccount.terminalHint", {
            defaultValue: "从卡片打开终端使用",
          })}
        </span>
      ) : null}
    </div>
  );
}
