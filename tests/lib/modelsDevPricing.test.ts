import { describe, expect, it } from "vitest";
import type {
  ModelPricing,
  ModelsDevSyncConfig,
} from "@/types/usage";
import {
  flattenModels,
  formatPrice,
  getCommonModelKeys,
  resolveModelsDevSelection,
  toModelPricing,
  type ModelsDevEntry,
} from "@/lib/modelsDevPricing";

function entry(fields: Partial<ModelsDevEntry>): ModelsDevEntry {
  return {
    key: fields.key ?? "provider/model",
    providerId: fields.providerId ?? "provider",
    providerName: fields.providerName ?? "Provider",
    modelId: fields.modelId ?? "model",
    normalizedId: fields.normalizedId ?? "model",
    modelName: fields.modelName ?? "Model",
    releaseDate: fields.releaseDate ?? "",
    input: fields.input ?? 0,
    output: fields.output ?? 0,
    cacheRead: fields.cacheRead ?? 0,
    cacheWrite: fields.cacheWrite ?? 0,
  };
}

describe("formatPrice", () => {
  it("renders nothing for values that carry no price", () => {
    for (const value of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(formatPrice(value)).toBe("0");
    }
  });

  it("renders nothing for absurd magnitudes", () => {
    expect(formatPrice(1e12)).toBe("0");
    expect(formatPrice(1e13)).toBe("0");
  });

  it("trims trailing zeros and keeps the useful digits", () => {
    expect(formatPrice(3)).toBe("3");
    expect(formatPrice(0.5)).toBe("0.5");
    expect(formatPrice(1.25)).toBe("1.25");
    expect(formatPrice(0.000001)).toBe("0.000001");
    expect(formatPrice(2.5)).toBe("2.5");
  });

  it("shows a six-decimal price without padding noise", () => {
    // 6 位小数就是精度边界：显示出来的不能再带多余的 0。
    expect(formatPrice(0.123456)).toBe("0.123456");
  });
});

describe("flattenModels", () => {
  it("keeps a priced text model and sorts newest first", () => {
    const data = {
      anthropic: {
        name: "Anthropic",
        models: {
          "claude-sonnet-4-5": {
            name: "Sonnet 4.5",
            release_date: "2025-09-29",
            cost: { input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 },
          },
          "claude-opus-4-1": {
            name: "Opus 4.1",
            release_date: "2025-08-05",
            cost: { input: 15, output: 75 },
          },
        },
      },
    };

    const entries = flattenModels(data);
    expect(entries).toHaveLength(2);
    expect(entries[0].modelId).toBe("claude-sonnet-4-5");
    expect(entries[0].releaseDate).toBe("2025-09-29");
    expect(entries[0].input).toBe(3);
    expect(entries[0].output).toBe(15);
    expect(entries[0].cacheRead).toBe(0.3);
    expect(entries[0].cacheWrite).toBe(3.75);
    expect(entries[1].modelId).toBe("claude-opus-4-1");
    // 没写的 cache 字段按 0 计，不影响主价格。
    expect(entries[1].cacheRead).toBe(0);
  });

  it("breaks a release-date tie by model name", () => {
    const data = {
      openai: {
        models: {
          "gpt-5.2": { name: "Zeta", release_date: "2026-01-01", cost: { input: 1 } },
          "gpt-5.1": { name: "Alpha", release_date: "2026-01-01", cost: { input: 1 } },
        },
      },
    };

    const entries = flattenModels(data);
    expect(entries.map((e) => e.modelId)).toEqual(["gpt-5.1", "gpt-5.2"]);
  });

  it("drops models that cannot price a text chat", () => {
    const data = {
      openai: {
        models: {
          "gpt-5.2": { name: "GPT", release_date: "2026-01-01", cost: { input: 1 } },
          // 非文本输出：不能拿来算用量。
          "gpt-image-1": {
            name: "Image",
            modalities: { output: ["image"] },
            cost: { input: 1 },
          },
          // 没有 text 输出。
          "gpt-realtime": {
            name: "Realtime",
            modalities: { output: [] },
            cost: { input: 1 },
          },
          // 已废弃。
          "gpt-4-old": {
            name: "GPT-4 old",
            status: "deprecated",
            cost: { input: 1 },
          },
          // 一点价格都没有。
          "gpt-5-free": { name: "GPT free", release_date: "2026-01-01" },
          // 名字带非文本标记。
          "tts-1": { name: "TTS", release_date: "2026-01-01", cost: { input: 1 } },
        },
      },
    };

    const entries = flattenModels(data);
    expect(entries.map((e) => e.modelId)).toEqual(["gpt-5.2"]);
  });

  it("drops a model whose normalized id is empty", () => {
    const data = {
      openai: {
        models: {
          "": { name: "Empty id", release_date: "2026-01-01", cost: { input: 1 } },
        },
      },
    };

    expect(flattenModels(data)).toEqual([]);
  });

  it("falls back to the raw id when the name is missing", () => {
    const data = {
      openai: {
        models: {
          "gpt-5.2": { release_date: "2026-01-01", cost: { input: 1 } },
        },
      },
    };

    const entries = flattenModels(data);
    expect(entries[0].modelName).toBe("gpt-5.2");
  });
});

describe("getCommonModelKeys", () => {
  // 调用方（flattenModels）传进来的就是按发布日期倒序的数组；这里照这个顺序造。
  const entries = [
    ...Array.from({ length: 8 }, (_, index) =>
      entry({
        key: `anthropic/claude-${7 - index}`,
        providerId: "anthropic",
        modelId: `claude-${7 - index}`,
        releaseDate: `2026-01-${String(8 - index).padStart(2, "0")}`,
      }),
    ),
    entry({ key: "anthropic/messages", providerId: "anthropic", modelId: "messages" }),
    entry({ key: "openai/gpt-5.2", providerId: "openai", modelId: "gpt-5.2" }),
    entry({ key: "openai/embed-me", providerId: "openai", modelId: "embed-me" }),
  ];

  it("caps one family at six recent models", () => {
    const keys = getCommonModelKeys(entries);
    const claude = [...keys].filter((key) => key.startsWith("anthropic/claude-"));
    expect(claude).toHaveLength(6);
    // 从数组头部截六个，最旧的两个（claude-0、claude-1）被丢掉。
    expect(keys.has("anthropic/claude-7")).toBe(true);
    expect(keys.has("anthropic/claude-2")).toBe(true);
    expect(keys.has("anthropic/claude-1")).toBe(false);
    expect(keys.has("anthropic/claude-0")).toBe(false);
  });

  it("picks in the order it is given, so the caller must pre-sort", () => {
    // 只依赖入组顺序、不自己排：把最旧的放前面，它就会被当成最近的挑走。
    const oldestFirst = [...entries].reverse();
    const keys = getCommonModelKeys(oldestFirst);
    expect(keys.has("anthropic/claude-0")).toBe(true);
    expect(keys.has("anthropic/claude-5")).toBe(true);
    expect(keys.has("anthropic/claude-6")).toBe(false);
  });

  it("keeps only the family's own provider and prefixes", () => {
    const keys = getCommonModelKeys(entries);
    expect(keys.has("anthropic/messages")).toBe(false);
    expect(keys.has("openai/gpt-5.2")).toBe(true);
    expect(keys.has("openai/embed-me")).toBe(false);
  });
});

describe("resolveModelsDevSelection", () => {
  const entries = [
    entry({ key: "anthropic/claude-1", providerId: "anthropic", modelId: "claude-1" }),
    entry({ key: "openai/gpt-5.2", providerId: "openai", modelId: "gpt-5.2" }),
    entry({ key: "openai/gpt-5.1", providerId: "openai", modelId: "gpt-5.1" }),
    entry({ key: "google/gemini-3", providerId: "google", modelId: "gemini-3" }),
  ];

  const config = (fields: Partial<ModelsDevSyncConfig>): ModelsDevSyncConfig =>
    ({
      autoSyncEnabled: false,
      includeCommonModels: false,
      selectedModelKeys: [],
      excludedCommonModelKeys: [],
      lastSyncAt: null,
      lastSyncError: null,
      ...fields,
    });

  it("returns nothing when nothing is picked and commons are off", () => {
    expect(resolveModelsDevSelection(entries, config({}))).toEqual([]);
  });

  it("always honours an explicit pick", () => {
    expect(
      resolveModelsDevSelection(
        entries,
        config({ selectedModelKeys: ["google/gemini-3"] }),
      ).map((e) => e.key),
    ).toEqual(["google/gemini-3"]);
  });

  it("adds the commons when the switch is on", () => {
    const picked = resolveModelsDevSelection(
      entries,
      config({
        selectedModelKeys: ["google/gemini-3"],
        includeCommonModels: true,
      }),
    ).map((e) => e.key);
    expect(picked).toContain("google/gemini-3");
    expect(picked).toContain("anthropic/claude-1");
    expect(picked).toContain("openai/gpt-5.2");
  });

  it("drops an excluded common that nothing explicitly picked", () => {
    const picked = resolveModelsDevSelection(
      entries,
      config({
        selectedModelKeys: ["google/gemini-3"],
        excludedCommonModelKeys: ["openai/gpt-5.1"],
        includeCommonModels: true,
      }),
    ).map((e) => e.key);
    expect(picked).toContain("google/gemini-3");
    expect(picked).not.toContain("openai/gpt-5.1");
    expect(picked).toContain("openai/gpt-5.2");
  });

  it("keeps an explicitly picked key even when the common entry is excluded", () => {
    const picked = resolveModelsDevSelection(
      entries,
      config({
        selectedModelKeys: ["openai/gpt-5.1"],
        excludedCommonModelKeys: ["openai/gpt-5.1"],
        includeCommonModels: true,
      }),
    ).map((e) => e.key);
    expect(picked).toContain("openai/gpt-5.1");
  });
});

describe("toModelPricing", () => {
  it("keeps the first row per normalized id and formats its prices", () => {
    const pricing = toModelPricing([
      entry({
        normalizedId: "gpt-5.2",
        modelName: "GPT 5.2",
        input: 1.25,
        output: 10,
        cacheRead: 0.125,
        cacheWrite: 1.5,
      }),
      entry({
        normalizedId: "gpt-5.2",
        modelName: "Duplicate",
        input: 99,
        output: 99,
      }),
      entry({
        normalizedId: "claude-1",
        modelName: "Claude",
        input: 3,
        output: 15,
      }),
    ]);

    expect(pricing).toHaveLength(2);
    const first = pricing.find((row) => row.modelId === "gpt-5.2") as ModelPricing;
    expect(first.displayName).toBe("GPT 5.2");
    expect(first.inputCostPerMillion).toBe("1.25");
    expect(first.outputCostPerMillion).toBe("10");
    expect(first.cacheReadCostPerMillion).toBe("0.125");
    expect(first.cacheCreationCostPerMillion).toBe("1.5");
  });

  it("returns nothing for no entries", () => {
    expect(toModelPricing([])).toEqual([]);
  });
});
