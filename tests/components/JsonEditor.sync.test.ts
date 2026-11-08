import { describe, expect, it } from "vitest";

import {
  mapPositionByContext,
  minimalTextChange,
  uniqueContextIndex,
} from "@/components/JsonEditor";

describe("minimalTextChange", () => {
  it("returns an empty edit when the text is unchanged", () => {
    expect(minimalTextChange("same", "same")).toEqual({
      from: 4,
      to: 4,
      insert: "",
    });
  });

  it("reports a pure append", () => {
    expect(minimalTextChange("abc", "abcdef")).toEqual({
      from: 3,
      to: 3,
      insert: "def",
    });
  });

  it("reports a pure prepend", () => {
    expect(minimalTextChange("abc", "Xabc")).toEqual({
      from: 0,
      to: 0,
      insert: "X",
    });
  });

  it("reports a middle replacement", () => {
    expect(minimalTextChange("hello world", "hello brave world")).toEqual({
      from: 6,
      to: 6,
      insert: "brave ",
    });
  });

  it("falls back to a full replacement when nothing is shared", () => {
    expect(minimalTextChange("abc", "xyz")).toEqual({
      from: 0,
      to: 3,
      insert: "xyz",
    });
  });
});

describe("uniqueContextIndex", () => {
  it("rejects an empty context", () => {
    expect(uniqueContextIndex("anything", "")).toBeNull();
  });

  it("rejects a missing context", () => {
    expect(uniqueContextIndex("anything", "zzz")).toBeNull();
  });

  it("returns the index when the context appears once", () => {
    expect(uniqueContextIndex("prefix-ctx-suffix", "ctx")).toBe(7);
  });

  it("rejects a context that appears more than once", () => {
    expect(uniqueContextIndex("ctx-x-ctx", "ctx")).toBeNull();
  });
});

describe("mapPositionByContext", () => {
  it("clamps a non-positive position to 0", () => {
    expect(mapPositionByContext("abc", "Xbc", 0)).toBe(0);
  });

  it("maps a position past the old end to the new length", () => {
    expect(mapPositionByContext("abc", "abcd", 3)).toBe(4);
  });

  it("keeps a position anchored on its unique right context", () => {
    // 光标在 "|def"，右边 "def" 在新文本里仍然唯一
    const current = "abcdef";
    const next = "abcXYdef";
    expect(mapPositionByContext(current, next, 3)).toBe(5);
  });

  it("keeps a position anchored on its unique left context", () => {
    // 光标在 "abc|"，左边 "abc" 在新文本里仍唯一，落点应含左上下文长度
    const current = "abcdef";
    const next = "Zabcdef";
    expect(mapPositionByContext(current, next, 3)).toBe(4);
  });

  it("returns null when no context is unique", () => {
    // 左右上下文在 next 里都重复出现：无法定位
    expect(mapPositionByContext("ab", "abab", 1)).toBeNull();
  });
});
