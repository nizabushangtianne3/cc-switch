import { afterEach, describe, expect, it, vi } from "vitest";
import { readLocalCache, writeLocalCache } from "@/lib/localCache";

const KEY = "prompts.claude.v1";

afterEach(() => {
  vi.restoreAllMocks();
  localStorage.clear();
});

describe("localCache", () => {
  it("round-trips a value through the prefixed key", () => {
    writeLocalCache(KEY, { prompts: { a: { name: "A" } } });
    // 前缀把它和调用方自己的键分开，避免撞车
    expect(localStorage.getItem("cc-switch-cache:" + KEY)).toBe(
      '{"prompts":{"a":{"name":"A"}}}',
    );
    expect(readLocalCache(KEY)).toEqual({ prompts: { a: { name: "A" } } });
  });

  it("returns undefined for a key that was never written", () => {
    expect(readLocalCache(KEY)).toBeUndefined();
  });

  it("returns undefined when the stored JSON is corrupt", () => {
    localStorage.setItem("cc-switch-cache:" + KEY, "{not json");
    expect(readLocalCache(KEY)).toBeUndefined();
  });

  it("returns undefined when reading throws", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("privacy mode");
    });
    expect(readLocalCache(KEY)).toBeUndefined();
  });

  it("swallows a write failure instead of propagating it", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });
    // 写不进去就让调用方照常从头加载，不能把加载流程带崩
    expect(() => writeLocalCache(KEY, { a: 1 })).not.toThrow();
    // 序列化仍然发生了：摔的是存储，不是数据准备
    expect(localStorage.setItem).toHaveBeenCalledWith(
      "cc-switch-cache:" + KEY,
      '{"a":1}',
    );
  });
});
