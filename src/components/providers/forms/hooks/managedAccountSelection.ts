import type { ManagedAuthAccount } from "@/lib/api/auth";

/**
 * 托管账号的「选中项是否可用」判定，ProviderForm 与 ClaudeDesktopProviderForm
 * 各有一份逐字相同的私有副本，已收成这里。
 *
 * 两者都走同一条兜底链：显式选中的账号 → 后端给的默认账号 → 列表里标了
 * is_default 的 → 列表第一个。Codex 与 xAI 只是「不可用」的判据不同
 * （reauth_required vs requires_reauth）。
 */

function effectiveAccountId(
  accountId: string | null,
  defaultAccountId: string | null,
  accounts: ManagedAuthAccount[],
): string | null {
  return (
    accountId ??
    defaultAccountId ??
    accounts.find((account) => account.is_default)?.id ??
    accounts[0]?.id ??
    null
  );
}

/** Codex 账号缺少身份或工作区元数据即不可用。 */
export function selectedCodexAccountIsUsable(
  accountId: string | null,
  defaultAccountId: string | null,
  accounts: ManagedAuthAccount[],
): boolean {
  const resolved = effectiveAccountId(accountId, defaultAccountId, accounts);
  return (
    !!resolved &&
    accounts.some(
      (account) => account.id === resolved && !account.reauth_required,
    )
  );
}

/** xAI 账号的刷新凭据失效即不可用。 */
export function selectedXaiAccountIsUsable(
  accountId: string | null,
  accounts: ManagedAuthAccount[],
): boolean {
  return (
    accountId === null ||
    accounts.some(
      (account) => account.id === accountId && !account.requires_reauth,
    )
  );
}

/** 选中的账号是否还在列表里；未选中（null）视为存在。 */
export function selectedAccountExists(
  accountId: string | null,
  accounts: Array<{ id: string }>,
): boolean {
  return (
    accountId === null || accounts.some((account) => account.id === accountId)
  );
}
