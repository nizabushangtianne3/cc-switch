import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useApiKeyStats } from "@/lib/query/usage";
import { TablePagination, useClientPagination } from "./TablePagination";
import { cn } from "@/lib/utils";
import {
  fmtInt,
  fmtUsd,
  formatTokensCompact,
  getLocaleFromLanguage,
  getResolvedLang,
} from "./format";
import { usageTable } from "./usageTable";
import { getUsageProviderLabel } from "./providerLabel";
import { SuccessSpeedCells, SuccessSpeedHeaders } from "./statsColumns";
import type { ApiKeyStats, UsageRangeSelection } from "@/types/usage";

interface ApiKeyStatsTableProps {
  range: UsageRangeSelection;
  appType?: string;
  providerName?: string;
  model?: string;
  apiKeyId?: string;
  refreshIntervalMs: number;
}

export function apiKeyStatsLabel(
  stat: ApiKeyStats,
  t: ReturnType<typeof useTranslation>["t"],
) {
  if (stat.keyIndex != null && stat.keyMask) {
    return t("usage.keyLabel.current", {
      index: stat.keyIndex,
      mask: stat.keyMask,
    });
  }
  return t("usage.keyLabel.deleted", {
    fingerprint: stat.apiKeyId.slice(0, 8),
  });
}

export function ApiKeyStatsTable({
  range,
  appType,
  providerName,
  model,
  apiKeyId,
  refreshIntervalMs,
}: ApiKeyStatsTableProps) {
  const { t, i18n } = useTranslation();
  const locale = getLocaleFromLanguage(getResolvedLang(i18n));
  const { data: stats, isLoading } = useApiKeyStats(
    range,
    { appType, providerName, model, apiKeyId },
    {
      refetchInterval: refreshIntervalMs > 0 ? refreshIntervalMs : false,
    },
  );

  const rows = useMemo(
    () => [...(stats ?? [])].sort((a, b) => b.requestCount - a.requestCount),
    [stats],
  );
  const pagination = useClientPagination(
    rows,
    JSON.stringify([range, appType, providerName, model, apiKeyId]),
  );

  if (isLoading) {
    return <div className={usageTable.skeleton} />;
  }

  return (
    <div className="flex flex-col">
      <div className={usageTable.scroller}>
        <table
          className={cn(usageTable.table, "min-w-[680px]")}
          aria-label={t("usage.keyStats")}
        >
          <thead>
            <tr className={usageTable.headRow}>
              <th className={usageTable.th}>{t("usage.keyColumn")}</th>
              <th className={usageTable.th}>{t("usage.requests")}</th>
              <th className={usageTable.th}>{t("usage.tokens")}</th>
              <th className={usageTable.th}>{t("usage.cost")}</th>
              <SuccessSpeedHeaders />
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 ? (
              <tr>
                <td colSpan={6} className={usageTable.empty}>
                  {t("usage.noData")}
                </td>
              </tr>
            ) : (
              pagination.pageRows.map((stat) => {
                const provider = getUsageProviderLabel(stat.providerName, t);
                const label = apiKeyStatsLabel(stat, t);
                return (
                  <tr
                    key={JSON.stringify([
                      stat.appType,
                      stat.providerId,
                      stat.apiKeyId,
                    ])}
                    className={usageTable.row}
                  >
                    <td className={usageTable.td}>
                      <span
                        className="block max-w-[360px] truncate"
                        title={`${provider.label} · ${label}`}
                      >
                        {label}
                      </span>
                      <span className="block max-w-[360px] truncate text-caption text-fg-3">
                        {provider.label}
                      </span>
                    </td>
                    <td className={usageTable.td}>
                      {fmtInt(stat.requestCount, locale)}
                    </td>
                    <td
                      className={usageTable.td}
                      title={fmtInt(stat.totalTokens, locale)}
                    >
                      {formatTokensCompact(stat.totalTokens, locale)}
                    </td>
                    <td
                      className={cn(usageTable.td, "font-medium")}
                      title={fmtUsd(stat.totalCost, 6)}
                    >
                      {fmtUsd(stat.totalCost, 2)}
                    </td>
                    <SuccessSpeedCells stat={stat} />
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
      <TablePagination
        page={pagination.page}
        totalPages={pagination.totalPages}
        total={pagination.total}
        onPageChange={pagination.setPage}
      />
    </div>
  );
}
