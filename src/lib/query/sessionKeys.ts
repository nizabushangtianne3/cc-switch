import type { ContentRef } from "@/types";

/** 键顺序无关的序列化：ContentRef / ImageRef 做 query key、缓存 key 用 */
export const stableKey = (value: unknown): string => {
  if (Array.isArray(value)) return `[${value.map(stableKey).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, item]) => item !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : 1))
      .map(([key, item]) => `${JSON.stringify(key)}:${stableKey(item)}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
};

export const sessionKeys = {
  messages: (providerId?: string, sourcePath?: string) =>
    ["sessionMessages", providerId, sourcePath] as const,
  transcript: (providerId?: string, sourcePath?: string) =>
    ["sessionTranscript", providerId, sourcePath] as const,
  blockContent: (providerId?: string, sourcePath?: string, ref?: ContentRef) =>
    [
      "sessionBlockContent",
      providerId,
      sourcePath,
      ref ? stableKey(ref) : null,
    ] as const,
};
