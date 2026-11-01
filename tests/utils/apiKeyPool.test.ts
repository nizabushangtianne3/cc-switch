import { describe, expect, it } from "vitest";
import {
  apiKeyEntriesFromText,
  normalizeApiKeyEntries,
  normalizeApiKeyWeight,
  splitApiKeyText,
} from "@/utils/apiKeyPool";

describe("apiKeyPool", () => {
  it("splits pasted keys from common list separators", () => {
    expect(splitApiKeyText("sk-a\nsk-b\r\nsk-c\tsk-d,sk-e;sk-f")).toEqual([
      "sk-a",
      "sk-b",
      "sk-c",
      "sk-d",
      "sk-e",
      "sk-f",
    ]);
  });

  it("migrates text to one entry per key", () => {
    expect(apiKeyEntriesFromText("sk-a\nsk-b")).toEqual([
      { key: "sk-a", weight: 1 },
      { key: "sk-b", weight: 1 },
    ]);
  });

  it("normalizes weights and removes blank keys", () => {
    expect(normalizeApiKeyWeight(undefined)).toBe(1);
    expect(normalizeApiKeyWeight(0)).toBe(1);
    expect(normalizeApiKeyWeight("2.9")).toBe(2);
    expect(
      normalizeApiKeyEntries([
        { key: " sk-a ", weight: 3 },
        { key: "   ", weight: 10 },
      ]),
    ).toEqual([{ key: "sk-a", weight: 3 }]);
  });
});
