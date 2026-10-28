import type { ApiKeyEntry, ApiKeyStrategy } from "@/types";

export const DEFAULT_API_KEY_STRATEGY: ApiKeyStrategy = "round_robin";

export const normalizeApiKeyWeight = (value: unknown): number => {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed) || parsed < 1) return 1;
  return Math.max(1, Math.floor(parsed));
};

export const splitApiKeyText = (value: string): string[] =>
  value
    .split(/\r?\n|\t|,|;/)
    .map((key) => key.trim())
    .filter(Boolean);

export const normalizeApiKeyEntries = (
  entries: ApiKeyEntry[] | undefined,
): ApiKeyEntry[] =>
  (entries ?? [])
    .map((entry) => ({
      key: typeof entry?.key === "string" ? entry.key.trim() : "",
      weight: normalizeApiKeyWeight(entry?.weight),
    }))
    .filter((entry) => entry.key !== "");

export const apiKeyEntriesFromText = (text: string): ApiKeyEntry[] =>
  splitApiKeyText(text).map((key) => ({ key, weight: 1 }));

export const apiKeyEntriesToText = (entries: ApiKeyEntry[]): string =>
  normalizeApiKeyEntries(entries)
    .map((entry) => entry.key)
    .join("\n");
